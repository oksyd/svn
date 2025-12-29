use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tokio::io::AsyncWriteExt;

use crate::editor::{
    AsyncEditorEventHandler, EditorEvent, EditorEventHandler, Report, ReportCommand,
};
use crate::options::UpdateOptions;
use crate::textdelta::{TextDeltaApplierFile, TextDeltaApplierFileSync};
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
/// - This editor uses blocking `std::fs` APIs. If you need async filesystem I/O,
///   see [`TokioFsEditor`].
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
        map_repo_path_to_fs(&self.root, self.strip_prefix.as_deref(), path, allow_empty)
    }

    fn new_tmp_path(&mut self, dest: &Path, token: &str) -> PathBuf {
        new_tmp_path(&self.root, dest, token, &mut self.next_tmp_id)
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

/// Applies `update`/`switch`/`replay`-style editor drives to a filesystem directory
/// using async `tokio::fs` I/O.
///
/// This editor implements [`AsyncEditorEventHandler`], so it can be driven via
/// methods like [`crate::RaSvnSession::update_with_async_handler`].
#[derive(Debug)]
pub struct TokioFsEditor {
    root: PathBuf,
    strip_prefix: Option<String>,
    dir_tokens: HashMap<String, PathBuf>,
    file_tokens: HashMap<String, PathBuf>,
    pending_files: HashMap<String, PendingFileAsync>,
    next_tmp_id: u64,
    #[cfg(unix)]
    exec_tokens: HashMap<String, bool>,
}

impl TokioFsEditor {
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
        map_repo_path_to_fs(&self.root, self.strip_prefix.as_deref(), path, allow_empty)
    }

    fn new_tmp_path(&mut self, dest: &Path, token: &str) -> PathBuf {
        new_tmp_path(&self.root, dest, token, &mut self.next_tmp_id)
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
                    tokio::fs::create_dir_all(&self.root).await?;
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
                    tokio::fs::create_dir_all(&dir).await?;
                    self.dir_tokens.insert(child_token, dir);
                    Ok(())
                }
                EditorEvent::CloseDir { dir_token } => {
                    let _ = self.dir_tokens.remove(&dir_token);
                    Ok(())
                }
                EditorEvent::DeleteEntry { path, .. } => {
                    let fs_path = self.repo_path_to_fs(&path, false)?;
                    if let Ok(meta) = tokio::fs::symlink_metadata(&fs_path).await {
                        if meta.is_dir() {
                            tokio::fs::remove_dir_all(&fs_path).await?;
                        } else {
                            tokio::fs::remove_file(&fs_path).await?;
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
                        tokio::fs::create_dir_all(parent).await?;
                    }
                    self.file_tokens.insert(file_token, file_path);
                    Ok(())
                }
                EditorEvent::ApplyTextDelta { file_token, .. } => {
                    let dest = self.file_tokens.get(&file_token).cloned().ok_or_else(|| {
                        SvnError::Protocol("apply-textdelta for unknown file token".into())
                    })?;

                    if let Ok(meta) = tokio::fs::symlink_metadata(&dest).await
                        && meta.file_type().is_symlink()
                    {
                        return Err(SvnError::InvalidPath(
                            "refusing to apply textdelta to a symlink".into(),
                        ));
                    }

                    let base = match tokio::fs::File::open(&dest).await {
                        Ok(file) => Some(file),
                        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
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
                        if let Some(exec) = self.exec_tokens.remove(&file_token) {
                            apply_executable_bit_async(&pending.dest, exec).await?;
                        }
                    } else if let Some(dest) = dest {
                        match tokio::fs::metadata(&dest).await {
                            Ok(_) => {}
                            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                                return Err(SvnError::Protocol(
                                    "close-file for missing file without textdelta".into(),
                                ));
                            }
                            Err(err) => return Err(err.into()),
                        }
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
                        let _ = tokio::fs::remove_file(pending.tmp).await;
                    }
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

#[cfg(unix)]
async fn apply_executable_bit_async(path: &Path, exec: bool) -> Result<(), SvnError> {
    use std::os::unix::fs::PermissionsExt;

    let mut perms = tokio::fs::metadata(path).await?.permissions();
    let mut mode = perms.mode();
    if exec {
        mode |= 0o111;
    } else {
        mode &= !0o111;
    }
    perms.set_mode(mode);
    tokio::fs::set_permissions(path, perms).await?;
    Ok(())
}

impl RaSvnSession {
    /// Exports a repository subtree to `dest` using `update`.
    ///
    /// This convenience helper builds a minimal "empty" report (`start_empty = true`)
    /// and drives [`crate::RaSvnSession::update_with_async_handler`] with a [`TokioFsEditor`].
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
        let mut editor = TokioFsEditor::new(dest.as_ref().to_path_buf())
            .with_strip_prefix(options.target.clone());
        self.update_with_async_handler(options, report, &mut editor)
            .await?;
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

fn map_repo_path_to_fs(
    root: &Path,
    strip_prefix: Option<&str>,
    path: &str,
    allow_empty: bool,
) -> Result<PathBuf, SvnError> {
    let mut trimmed = path.trim().trim_start_matches('/');

    if let Some(prefix) = strip_prefix {
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
            return Ok(root.to_path_buf());
        }
        return Err(SvnError::InvalidPath("empty path".into()));
    }

    if trimmed.contains('\\') || trimmed.contains(':') {
        return Err(SvnError::InvalidPath("unsafe path".into()));
    }

    let mut out = root.to_path_buf();
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

fn new_tmp_path(root: &Path, dest: &Path, token: &str, next_tmp_id: &mut u64) -> PathBuf {
    let parent = dest.parent().unwrap_or(root);
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

    *next_tmp_id = next_tmp_id.wrapping_add(1);
    parent.join(format!(".svn-rs.{name}.{token}.{}.tmp", *next_tmp_id))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use std::future::Future;

    #[test]
    fn fs_editor_rejects_parent_dir_paths() {
        let editor = FsEditor::new("tmp");
        assert!(editor.repo_path_to_fs("../x", false).is_err());
        assert!(editor.repo_path_to_fs("a/../x", false).is_err());
    }

    #[test]
    fn tokio_fs_editor_rejects_parent_dir_paths() {
        let editor = TokioFsEditor::new("tmp");
        assert!(editor.repo_path_to_fs("../x", false).is_err());
        assert!(editor.repo_path_to_fs("a/../x", false).is_err());
    }

    fn run_async<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[test]
    fn tokio_fs_editor_writes_fulltext_delta_to_disk() {
        run_async(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();

            let mut editor = TokioFsEditor::new(root.clone());
            editor
                .on_event(EditorEvent::OpenRoot {
                    rev: None,
                    token: "d0".to_string(),
                })
                .await
                .unwrap();
            editor
                .on_event(EditorEvent::AddFile {
                    path: "hello.txt".to_string(),
                    dir_token: "d0".to_string(),
                    file_token: "f1".to_string(),
                    copy_from: None,
                })
                .await
                .unwrap();
            editor
                .on_event(EditorEvent::ApplyTextDelta {
                    file_token: "f1".to_string(),
                    base_checksum: None,
                })
                .await
                .unwrap();

            let delta = crate::svndiff::encode_fulltext_with_options(
                crate::svndiff::SvndiffVersion::V0,
                b"hello",
                0,
                1024,
            )
            .unwrap();
            editor
                .on_event(EditorEvent::TextDeltaChunk {
                    file_token: "f1".to_string(),
                    chunk: delta,
                })
                .await
                .unwrap();
            editor
                .on_event(EditorEvent::TextDeltaEnd {
                    file_token: "f1".to_string(),
                })
                .await
                .unwrap();
            editor
                .on_event(EditorEvent::CloseFile {
                    file_token: "f1".to_string(),
                    text_checksum: None,
                })
                .await
                .unwrap();
            editor.on_event(EditorEvent::CloseEdit).await.unwrap();

            let written = tokio::fs::read(root.join("hello.txt")).await.unwrap();
            assert_eq!(written, b"hello");
        });
    }
}
