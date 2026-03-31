#[cfg(unix)]
use super::shared::apply_executable_bit;
use super::shared::{
    ExportState, copy_dir_missing_recursive, create_dir_all_no_symlink, ensure_no_symlink_prefix,
    is_symlink_like,
};
use super::*;

/// Applies `update`/`switch`/`replay`-style editor drives to a filesystem directory.
///
/// `FsEditor` is a thin "export" helper: it materializes files and directories,
/// but it does **not** implement a full Subversion working copy.
///
/// ## Notes
///
/// - Paths from the server are treated as repository-relative (`/`-separated)
///   and are validated to avoid directory traversal.
/// - Writes are refused beneath symlinks (or Windows reparse points) under
///   `root`, to avoid writing outside the export directory.
/// - Text deltas are applied to the current on-disk file as the base. For a
///   fresh export, use a report with `start_empty = true` so servers typically
///   send fulltext deltas.
/// - This editor uses blocking `std::fs` APIs. If you need async filesystem I/O,
///   see [`TokioFsEditor`].
#[derive(Debug)]
pub struct FsEditor {
    state: ExportState,
    pending_files: HashMap<String, PendingFile>,
}

impl FsEditor {
    /// Creates a filesystem editor rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            state: ExportState::new(root),
            pending_files: HashMap::new(),
        }
    }

    /// Returns the export root directory.
    pub fn root(&self) -> &Path {
        self.state.root()
    }

    /// Strips `prefix` from incoming editor paths if present.
    ///
    /// This is useful when the server emits repository-relative paths, but the
    /// caller wants `root` to correspond to a specific subtree (for example
    /// exporting `trunk/` into an empty directory).
    #[must_use]
    pub fn with_strip_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.state = self.state.with_strip_prefix(prefix);
        self
    }

    pub(super) fn repo_path_to_fs(
        &self,
        path: &str,
        allow_empty: bool,
    ) -> Result<PathBuf, SvnError> {
        self.state.repo_path_to_fs(path, allow_empty)
    }

    fn new_tmp_path(&mut self, dest: &Path, token: &str) -> PathBuf {
        self.state.new_tmp_path(dest, token)
    }
}

impl Default for FsEditor {
    fn default() -> Self {
        Self::new(".")
    }
}

impl EditorEventHandler for FsEditor {
    fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
        match event {
            EditorEvent::TargetRev { .. } => Ok(()),
            EditorEvent::OpenRoot { token, .. } => {
                create_dir_all_no_symlink(self.state.root(), self.state.root())?;
                self.state.open_root(token);
                Ok(())
            }
            EditorEvent::AddDir {
                path,
                child_token,
                copy_from,
                ..
            } => {
                let dir = self.repo_path_to_fs(&path, true)?;
                create_dir_all_no_symlink(self.state.root(), &dir)?;
                if let Some((src_path, _src_rev)) = copy_from {
                    let src = self.repo_path_to_fs(&src_path, true)?;
                    ensure_no_symlink_prefix(self.state.root(), &src)?;
                    if src.exists() {
                        copy_dir_missing_recursive(&src, &dir)?;
                    }
                }
                self.state.open_dir(child_token, dir);
                Ok(())
            }
            EditorEvent::OpenDir {
                path, child_token, ..
            } => {
                let dir = self.repo_path_to_fs(&path, true)?;
                create_dir_all_no_symlink(self.state.root(), &dir)?;
                self.state.open_dir(child_token, dir);
                Ok(())
            }
            EditorEvent::CloseDir { dir_token } => {
                self.state.close_dir(&dir_token);
                Ok(())
            }
            EditorEvent::DeleteEntry { path, .. } => {
                let fs_path = self.repo_path_to_fs(&path, false)?;
                if let Some(parent) = fs_path.parent() {
                    ensure_no_symlink_prefix(self.state.root(), parent)?;
                }
                if let Ok(meta) = std::fs::symlink_metadata(&fs_path) {
                    if is_symlink_like(&meta) {
                        if meta.is_dir() {
                            std::fs::remove_dir(&fs_path)?;
                        } else {
                            std::fs::remove_file(&fs_path)?;
                        }
                    } else if meta.is_dir() {
                        std::fs::remove_dir_all(&fs_path)?;
                    } else {
                        std::fs::remove_file(&fs_path)?;
                    }
                }
                Ok(())
            }
            EditorEvent::AbsentDir { .. } | EditorEvent::AbsentFile { .. } => Ok(()),
            EditorEvent::AddFile {
                path,
                file_token,
                copy_from,
                ..
            } => {
                let file_path = self.repo_path_to_fs(&path, false)?;
                if let Some(parent) = file_path.parent() {
                    create_dir_all_no_symlink(self.state.root(), parent)?;
                }
                let copy_from = match copy_from {
                    Some((src_path, _src_rev)) => {
                        let src = self.repo_path_to_fs(&src_path, false)?;
                        ensure_no_symlink_prefix(self.state.root(), &src)?;
                        Some(src)
                    }
                    None => None,
                };
                self.state.track_file(file_token, file_path, copy_from);
                Ok(())
            }
            EditorEvent::OpenFile {
                path, file_token, ..
            } => {
                let file_path = self.repo_path_to_fs(&path, false)?;
                if let Some(parent) = file_path.parent() {
                    create_dir_all_no_symlink(self.state.root(), parent)?;
                }
                self.state.track_file(file_token, file_path, None);
                Ok(())
            }
            EditorEvent::ApplyTextDelta { file_token, .. } => {
                let dest = self.state.file_dest(&file_token)?;

                ensure_no_symlink_prefix(self.state.root(), &dest)?;

                if let Ok(meta) = std::fs::symlink_metadata(&dest)
                    && is_symlink_like(&meta)
                {
                    return Err(SvnError::InvalidPath(
                        "refusing to apply textdelta to a symlink/reparse point".into(),
                    ));
                }

                let base = match File::open(&dest) {
                    Ok(file) => Some(file),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                        if let Some(src) = self.state.file_copy_source(&file_token) {
                            ensure_no_symlink_prefix(self.state.root(), src)?;
                            match File::open(src) {
                                Ok(file) => Some(file),
                                Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                                Err(err) => return Err(err.into()),
                            }
                        } else {
                            None
                        }
                    }
                    Err(err) => return Err(err.into()),
                };
                let applier = TextDeltaApplierFileSync::new(base)?;

                let tmp = self.new_tmp_path(&dest, &file_token);
                let out = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp)?;

                self.pending_files.insert(
                    file_token,
                    PendingFile {
                        dest,
                        tmp,
                        out,
                        applier: Some(applier),
                        delta_ended: false,
                    },
                );
                Ok(())
            }
            EditorEvent::TextDeltaChunk { file_token, chunk } => {
                let pending = self.pending_files.get_mut(&file_token).ok_or_else(|| {
                    SvnError::Protocol("textdelta-chunk without apply-textdelta".into())
                })?;

                let applier = pending.applier.as_mut().ok_or_else(|| {
                    SvnError::Protocol("textdelta-chunk after textdelta-end".into())
                })?;
                applier.push(&chunk, &mut pending.out)?;
                Ok(())
            }
            EditorEvent::TextDeltaEnd { file_token } => {
                let pending = self.pending_files.get_mut(&file_token).ok_or_else(|| {
                    SvnError::Protocol("textdelta-end without apply-textdelta".into())
                })?;

                let applier = pending
                    .applier
                    .take()
                    .ok_or_else(|| SvnError::Protocol("duplicate textdelta-end".into()))?;
                applier.finish(&mut pending.out)?;
                pending.delta_ended = true;
                Ok(())
            }
            EditorEvent::ChangeFileProp {
                file_token,
                name,
                value,
            } => {
                #[cfg(unix)]
                if name == "svn:executable" {
                    self.state.record_exec(&file_token, value.is_some());
                }
                let _ = (file_token, name, value);
                Ok(())
            }
            EditorEvent::ChangeDirProp { .. } => Ok(()),
            EditorEvent::CloseFile { file_token, .. } => {
                let dest = self.state.file_dest_if_known(&file_token);
                let pending = self.pending_files.remove(&file_token);

                if let Some(mut pending) = pending {
                    ensure_no_symlink_prefix(self.state.root(), &pending.dest)?;

                    if pending.applier.is_some() || !pending.delta_ended {
                        return Err(SvnError::Protocol("close-file before textdelta-end".into()));
                    }

                    pending.out.flush()?;
                    drop(pending.out);

                    if pending.dest.exists() {
                        let _ = std::fs::remove_file(&pending.dest);
                    }
                    std::fs::rename(&pending.tmp, &pending.dest)?;

                    #[cfg(unix)]
                    if let Some(exec) = self.state.take_exec(&file_token) {
                        apply_executable_bit(&pending.dest, exec)?;
                    }
                } else if let Some(dest) = dest
                    && !dest.exists()
                {
                    if let Some(src) = self.state.file_copy_source(&file_token)
                        && src.exists()
                    {
                        ensure_no_symlink_prefix(self.state.root(), src)?;
                        ensure_no_symlink_prefix(self.state.root(), &dest)?;
                        if let Ok(meta) = std::fs::symlink_metadata(&dest)
                            && is_symlink_like(&meta)
                        {
                            return Err(SvnError::InvalidPath(
                                "refusing to copy a file over a symlink/reparse point".into(),
                            ));
                        }
                        let _ = std::fs::copy(src, &dest)?;
                    } else {
                        return Err(SvnError::Protocol(
                            "close-file for missing file without textdelta".into(),
                        ));
                    }
                }

                self.state.clear_file(&file_token);
                Ok(())
            }
            EditorEvent::CloseEdit => {
                if !self.pending_files.is_empty() {
                    return Err(SvnError::Protocol(
                        "close-edit with pending textdeltas".into(),
                    ));
                }
                self.state.reset();
                Ok(())
            }
            EditorEvent::AbortEdit => {
                for (_, pending) in self.pending_files.drain() {
                    let _ = std::fs::remove_file(pending.tmp);
                }
                self.state.reset();
                Ok(())
            }
            EditorEvent::FinishReplay | EditorEvent::RevProps { .. } => Ok(()),
        }
    }
}

#[derive(Debug)]
struct PendingFile {
    dest: PathBuf,
    tmp: PathBuf,
    out: std::fs::File,
    applier: Option<TextDeltaApplierFileSync>,
    delta_ended: bool,
}
