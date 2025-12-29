use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::editor::{EditorEvent, EditorEventHandler, Report, ReportCommand};
use crate::options::UpdateOptions;
use crate::textdelta::TextDeltaApplierFileSync;
use crate::{RaSvnClient, RaSvnSession, SvnError};

/// Applies `update`/`switch`/`replay`-style editor drives to a filesystem directory.
///
/// `FsEditor` is a thin "export" helper: it materializes files and directories,
/// but it does **not** implement a full Subversion working copy.
///
/// ## Notes
///
/// - Paths from the server are treated as repository-relative (`/`-separated)
///   and are validated to avoid directory traversal.
/// - Text deltas are applied to the current on-disk file as the base. For a
///   fresh export, use a report with `start_empty = true` so servers typically
///   send fulltext deltas.
#[derive(Debug)]
pub struct FsEditor {
    root: PathBuf,
    strip_prefix: Option<String>,
    dir_tokens: HashMap<String, PathBuf>,
    file_tokens: HashMap<String, PathBuf>,
    pending_files: HashMap<String, PendingFile>,
    next_tmp_id: u64,
    #[cfg(unix)]
    exec_tokens: HashMap<String, bool>,
}

impl FsEditor {
    /// Creates a filesystem editor rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            strip_prefix: None,
            dir_tokens: HashMap::new(),
            file_tokens: HashMap::new(),
            pending_files: HashMap::new(),
            next_tmp_id: 0,
            #[cfg(unix)]
            exec_tokens: HashMap::new(),
        }
    }

    /// Returns the export root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Strips `prefix` from incoming editor paths if present.
    ///
    /// This is useful when the server emits repository-relative paths, but the
    /// caller wants `root` to correspond to a specific subtree (for example
    /// exporting `trunk/` into an empty directory).
    #[must_use]
    pub fn with_strip_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.strip_prefix = Some(prefix.into());
        self
    }

    fn repo_path_to_fs(&self, path: &str, allow_empty: bool) -> Result<PathBuf, SvnError> {
        let mut trimmed = path.trim().trim_start_matches('/');

        if let Some(prefix) = self.strip_prefix.as_deref() {
            let prefix = prefix.trim().trim_matches('/');
            if !prefix.is_empty() {
                if trimmed == prefix {
                    trimmed = "";
                } else if let Some(rest) = trimmed.strip_prefix(prefix)
                    && let Some(rest) = rest.strip_prefix('/')
                {
                    trimmed = rest;
                }
            }
        }

        if trimmed.is_empty() {
            if allow_empty {
                return Ok(self.root.clone());
            }
            return Err(SvnError::InvalidPath("empty path".into()));
        }

        if trimmed.contains('\\') || trimmed.contains(':') {
            return Err(SvnError::InvalidPath("unsafe path".into()));
        }

        let mut out = self.root.clone();
        for part in trimmed.split('/') {
            if part.is_empty()
                || part == "."
                || part == ".."
                || part.contains('\\')
                || part.contains(':')
            {
                return Err(SvnError::InvalidPath("unsafe path".into()));
            }
            out.push(part);
        }
        Ok(out)
    }

    fn new_tmp_path(&mut self, dest: &Path, token: &str) -> PathBuf {
        let parent = dest.parent().unwrap_or(&self.root);
        let mut name = dest
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());

        name.retain(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
        if name.is_empty() {
            name = "file".to_string();
        }

        let token: String = token
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();

        self.next_tmp_id = self.next_tmp_id.wrapping_add(1);
        parent.join(format!(".svn-rs.{name}.{token}.{}.tmp", self.next_tmp_id))
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
                std::fs::create_dir_all(&self.root)?;
                self.dir_tokens.insert(token, self.root.clone());
                Ok(())
            }
            EditorEvent::AddDir {
                path, child_token, ..
            }
            | EditorEvent::OpenDir {
                path, child_token, ..
            } => {
                let dir = self.repo_path_to_fs(&path, true)?;
                std::fs::create_dir_all(&dir)?;
                self.dir_tokens.insert(child_token, dir);
                Ok(())
            }
            EditorEvent::CloseDir { dir_token } => {
                let _ = self.dir_tokens.remove(&dir_token);
                Ok(())
            }
            EditorEvent::DeleteEntry { path, .. } => {
                let fs_path = self.repo_path_to_fs(&path, false)?;
                if let Ok(meta) = std::fs::symlink_metadata(&fs_path) {
                    if meta.is_dir() {
                        std::fs::remove_dir_all(&fs_path)?;
                    } else {
                        std::fs::remove_file(&fs_path)?;
                    }
                }
                Ok(())
            }
            EditorEvent::AbsentDir { .. } | EditorEvent::AbsentFile { .. } => Ok(()),
            EditorEvent::AddFile {
                path, file_token, ..
            }
            | EditorEvent::OpenFile {
                path, file_token, ..
            } => {
                let file_path = self.repo_path_to_fs(&path, false)?;
                if let Some(parent) = file_path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                self.file_tokens.insert(file_token, file_path);
                Ok(())
            }
            EditorEvent::ApplyTextDelta { file_token, .. } => {
                let dest = self.file_tokens.get(&file_token).cloned().ok_or_else(|| {
                    SvnError::Protocol("apply-textdelta for unknown file token".into())
                })?;

                if let Ok(meta) = std::fs::symlink_metadata(&dest)
                    && meta.file_type().is_symlink()
                {
                    return Err(SvnError::InvalidPath(
                        "refusing to apply textdelta to a symlink".into(),
                    ));
                }

                let base = match File::open(&dest) {
                    Ok(file) => Some(file),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
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
                    self.exec_tokens.insert(file_token.clone(), value.is_some());
                }
                let _ = (file_token, name, value);
                Ok(())
            }
            EditorEvent::ChangeDirProp { .. } => Ok(()),
            EditorEvent::CloseFile { file_token, .. } => {
                let dest = self.file_tokens.get(&file_token).cloned();
                let pending = self.pending_files.remove(&file_token);

                if let Some(mut pending) = pending {
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
                    if let Some(exec) = self.exec_tokens.remove(&file_token) {
                        apply_executable_bit(&pending.dest, exec)?;
                    }
                } else if let Some(dest) = dest
                    && !dest.exists()
                {
                    return Err(SvnError::Protocol(
                        "close-file for missing file without textdelta".into(),
                    ));
                }

                let _ = self.file_tokens.remove(&file_token);
                Ok(())
            }
            EditorEvent::CloseEdit => {
                if !self.pending_files.is_empty() {
                    return Err(SvnError::Protocol(
                        "close-edit with pending textdeltas".into(),
                    ));
                }
                Ok(())
            }
            EditorEvent::AbortEdit => {
                for (_, pending) in self.pending_files.drain() {
                    let _ = std::fs::remove_file(pending.tmp);
                }
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

#[cfg(unix)]
fn apply_executable_bit(path: &Path, exec: bool) -> Result<(), SvnError> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = std::fs::metadata(path)?.permissions();
    let mut mode = perms.mode();
    if exec {
        mode |= 0o111;
    } else {
        mode &= !0o111;
    }
    perms.set_mode(mode);
    std::fs::set_permissions(path, perms)?;
    Ok(())
}

impl RaSvnSession {
    /// Exports a repository subtree to `dest` using `update`.
    ///
    /// This convenience helper builds a minimal "empty" report (`start_empty = true`)
    /// and drives [`crate::RaSvnSession::update`] with an [`FsEditor`].
    pub async fn export_to_dir(
        &mut self,
        options: &UpdateOptions,
        dest: impl AsRef<Path>,
    ) -> Result<(), SvnError> {
        let mut report = Report::new();
        report.push(ReportCommand::SetPath {
            path: String::new(),
            rev: 0,
            start_empty: true,
            lock_token: None,
            depth: options.depth,
        });
        report.finish();

        self.export_to_dir_with_report(options, &report, dest).await
    }

    /// Exports a repository subtree to `dest` using a caller-provided report.
    pub async fn export_to_dir_with_report(
        &mut self,
        options: &UpdateOptions,
        report: &Report,
        dest: impl AsRef<Path>,
    ) -> Result<(), SvnError> {
        let mut editor =
            FsEditor::new(dest.as_ref().to_path_buf()).with_strip_prefix(options.target.clone());
        self.update(options, report, &mut editor).await?;
        Ok(())
    }
}

impl RaSvnClient {
    /// Convenience wrapper for [`RaSvnSession::export_to_dir`].
    pub async fn export_to_dir(
        &self,
        options: &UpdateOptions,
        dest: impl AsRef<Path>,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.export_to_dir(options, dest).await
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn fs_editor_rejects_parent_dir_paths() {
        let editor = FsEditor::new("tmp");
        assert!(editor.repo_path_to_fs("../x", false).is_err());
        assert!(editor.repo_path_to_fs("a/../x", false).is_err());
    }
}
