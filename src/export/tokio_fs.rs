#[cfg(unix)]
use super::shared::apply_executable_bit_async;
use super::shared::{
    ExportState, copy_dir_missing_recursive_async, create_dir_all_no_symlink_async,
    ensure_no_symlink_prefix_async, is_symlink_like,
};
use super::*;

/// Applies `update`/`switch`/`replay`-style editor drives to a filesystem directory
/// using async `tokio::fs` I/O.
///
/// This editor implements [`AsyncEditorEventHandler`], so it can be driven via
/// methods like [`crate::RaSvnSession::update_with_async_handler`].
///
/// Like [`FsEditor`], it refuses writes through symlinks (or Windows reparse
/// points) under `root`.
#[derive(Debug)]
pub struct TokioFsEditor {
    state: ExportState,
    pending_files: HashMap<String, PendingFileAsync>,
}

impl TokioFsEditor {
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

impl Default for TokioFsEditor {
    fn default() -> Self {
        Self::new(".")
    }
}

impl AsyncEditorEventHandler for TokioFsEditor {
    fn on_event<'a>(
        &'a mut self,
        event: EditorEvent,
    ) -> Pin<Box<dyn Future<Output = Result<(), SvnError>> + Send + 'a>> {
        Box::pin(async move {
            match event {
                EditorEvent::TargetRev { .. } => Ok(()),
                EditorEvent::OpenRoot { token, .. } => {
                    create_dir_all_no_symlink_async(self.state.root(), self.state.root()).await?;
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
                    create_dir_all_no_symlink_async(self.state.root(), &dir).await?;
                    if let Some((src_path, _src_rev)) = copy_from {
                        let src = self.repo_path_to_fs(&src_path, true)?;
                        ensure_no_symlink_prefix_async(self.state.root(), &src).await?;
                        if tokio::fs::metadata(&src).await.is_ok() {
                            copy_dir_missing_recursive_async(&src, &dir).await?;
                        }
                    }
                    self.state.open_dir(child_token, dir);
                    Ok(())
                }
                EditorEvent::OpenDir {
                    path, child_token, ..
                } => {
                    let dir = self.repo_path_to_fs(&path, true)?;
                    create_dir_all_no_symlink_async(self.state.root(), &dir).await?;
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
                        ensure_no_symlink_prefix_async(self.state.root(), parent).await?;
                    }
                    if let Ok(meta) = tokio::fs::symlink_metadata(&fs_path).await {
                        if is_symlink_like(&meta) {
                            if meta.is_dir() {
                                tokio::fs::remove_dir(&fs_path).await?;
                            } else {
                                tokio::fs::remove_file(&fs_path).await?;
                            }
                        } else if meta.is_dir() {
                            tokio::fs::remove_dir_all(&fs_path).await?;
                        } else {
                            tokio::fs::remove_file(&fs_path).await?;
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
                        create_dir_all_no_symlink_async(self.state.root(), parent).await?;
                    }
                    let copy_from = match copy_from {
                        Some((src_path, _src_rev)) => {
                            let src = self.repo_path_to_fs(&src_path, false)?;
                            ensure_no_symlink_prefix_async(self.state.root(), &src).await?;
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
                        create_dir_all_no_symlink_async(self.state.root(), parent).await?;
                    }
                    self.state.track_file(file_token, file_path, None);
                    Ok(())
                }
                EditorEvent::ApplyTextDelta { file_token, .. } => {
                    let dest = self.state.file_dest(&file_token)?;

                    ensure_no_symlink_prefix_async(self.state.root(), &dest).await?;

                    if let Ok(meta) = tokio::fs::symlink_metadata(&dest).await
                        && is_symlink_like(&meta)
                    {
                        return Err(SvnError::InvalidPath(
                            "refusing to apply textdelta to a symlink/reparse point".into(),
                        ));
                    }

                    let base = match tokio::fs::File::open(&dest).await {
                        Ok(file) => Some(file),
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                            if let Some(src) = self.state.file_copy_source(&file_token) {
                                ensure_no_symlink_prefix_async(self.state.root(), src).await?;
                                match tokio::fs::File::open(src).await {
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
                    let applier = TextDeltaApplierFile::new(base).await?;

                    let tmp = self.new_tmp_path(&dest, &file_token);
                    let out = tokio::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&tmp)
                        .await?;

                    self.pending_files.insert(
                        file_token,
                        PendingFileAsync {
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
                    applier.push(&chunk, &mut pending.out).await?;
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
                    applier.finish(&mut pending.out).await?;
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
                        ensure_no_symlink_prefix_async(self.state.root(), &pending.dest).await?;

                        if pending.applier.is_some() || !pending.delta_ended {
                            return Err(SvnError::Protocol(
                                "close-file before textdelta-end".into(),
                            ));
                        }

                        pending.out.flush().await?;
                        drop(pending.out);

                        match tokio::fs::remove_file(&pending.dest).await {
                            Ok(()) => {}
                            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                            Err(err) => return Err(err.into()),
                        }
                        tokio::fs::rename(&pending.tmp, &pending.dest).await?;

                        #[cfg(unix)]
                        if let Some(exec) = self.state.take_exec(&file_token) {
                            apply_executable_bit_async(&pending.dest, exec).await?;
                        }
                    } else if let Some(dest) = dest {
                        match tokio::fs::metadata(&dest).await {
                            Ok(_) => {}
                            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                                match self.state.file_copy_source(&file_token) {
                                    Some(src) => {
                                        ensure_no_symlink_prefix_async(self.state.root(), src)
                                            .await?;
                                        if tokio::fs::metadata(src).await.is_ok() {
                                            ensure_no_symlink_prefix_async(
                                                self.state.root(),
                                                &dest,
                                            )
                                            .await?;

                                            if let Ok(meta) =
                                                tokio::fs::symlink_metadata(&dest).await
                                                && is_symlink_like(&meta)
                                            {
                                                return Err(SvnError::InvalidPath(
                                                    "refusing to copy a file over a symlink/reparse point"
                                                        .into(),
                                                ));
                                            }
                                            let _ = tokio::fs::copy(src, &dest).await?;
                                        } else {
                                            return Err(SvnError::Protocol(
                                                "close-file for missing file without textdelta"
                                                    .into(),
                                            ));
                                        }
                                    }
                                    None => {
                                        return Err(SvnError::Protocol(
                                            "close-file for missing file without textdelta".into(),
                                        ));
                                    }
                                }
                            }
                            Err(err) => return Err(err.into()),
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
                        let _ = tokio::fs::remove_file(pending.tmp).await;
                    }
                    self.state.reset();
                    Ok(())
                }
                EditorEvent::FinishReplay | EditorEvent::RevProps { .. } => Ok(()),
            }
        })
    }
}

#[derive(Debug)]
struct PendingFileAsync {
    dest: PathBuf,
    tmp: PathBuf,
    out: tokio::fs::File,
    applier: Option<TextDeltaApplierFile>,
    delta_ended: bool,
}
