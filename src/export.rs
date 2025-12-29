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
/// - Writes are refused beneath symlinks (or Windows reparse points) under
///   `root`, to avoid writing outside the export directory.
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
    file_copy_from: HashMap<String, PathBuf>,
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
            file_copy_from: HashMap::new(),
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
                create_dir_all_no_symlink(&self.root, &self.root)?;
                self.dir_tokens.insert(token, self.root.clone());
                Ok(())
            }
            EditorEvent::AddDir {
                path,
                child_token,
                copy_from,
                ..
            } => {
                let dir = self.repo_path_to_fs(&path, true)?;
                create_dir_all_no_symlink(&self.root, &dir)?;
                if let Some((src_path, _src_rev)) = copy_from {
                    let src = self.repo_path_to_fs(&src_path, true)?;
                    ensure_no_symlink_prefix(&self.root, &src)?;
                    if src.exists() {
                        copy_dir_missing_recursive(&src, &dir)?;
                    }
                }
                self.dir_tokens.insert(child_token, dir);
                Ok(())
            }
            EditorEvent::OpenDir {
                path, child_token, ..
            } => {
                let dir = self.repo_path_to_fs(&path, true)?;
                create_dir_all_no_symlink(&self.root, &dir)?;
                self.dir_tokens.insert(child_token, dir);
                Ok(())
            }
            EditorEvent::CloseDir { dir_token } => {
                let _ = self.dir_tokens.remove(&dir_token);
                Ok(())
            }
            EditorEvent::DeleteEntry { path, .. } => {
                let fs_path = self.repo_path_to_fs(&path, false)?;
                if let Some(parent) = fs_path.parent() {
                    ensure_no_symlink_prefix(&self.root, parent)?;
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
                    create_dir_all_no_symlink(&self.root, parent)?;
                }
                if let Some((src_path, _src_rev)) = copy_from {
                    let src = self.repo_path_to_fs(&src_path, false)?;
                    ensure_no_symlink_prefix(&self.root, &src)?;
                    self.file_copy_from.insert(file_token.clone(), src.clone());
                }
                self.file_tokens.insert(file_token, file_path);
                Ok(())
            }
            EditorEvent::OpenFile {
                path, file_token, ..
            } => {
                let file_path = self.repo_path_to_fs(&path, false)?;
                if let Some(parent) = file_path.parent() {
                    create_dir_all_no_symlink(&self.root, parent)?;
                }
                self.file_tokens.insert(file_token, file_path);
                Ok(())
            }
            EditorEvent::ApplyTextDelta { file_token, .. } => {
                let dest = self.file_tokens.get(&file_token).cloned().ok_or_else(|| {
                    SvnError::Protocol("apply-textdelta for unknown file token".into())
                })?;

                ensure_no_symlink_prefix(&self.root, &dest)?;

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
                        if let Some(src) = self.file_copy_from.get(&file_token) {
                            ensure_no_symlink_prefix(&self.root, src)?;
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
                    ensure_no_symlink_prefix(&self.root, &pending.dest)?;

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
                    if let Some(src) = self.file_copy_from.get(&file_token)
                        && src.exists()
                    {
                        ensure_no_symlink_prefix(&self.root, src)?;
                        ensure_no_symlink_prefix(&self.root, &dest)?;
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

                let _ = self.file_copy_from.remove(&file_token);
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
                self.file_copy_from.clear();
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
///
/// Like [`FsEditor`], it refuses writes through symlinks (or Windows reparse
/// points) under `root`.
#[derive(Debug)]
pub struct TokioFsEditor {
    root: PathBuf,
    strip_prefix: Option<String>,
    dir_tokens: HashMap<String, PathBuf>,
    file_tokens: HashMap<String, PathBuf>,
    file_copy_from: HashMap<String, PathBuf>,
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
            file_copy_from: HashMap::new(),
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
                    create_dir_all_no_symlink_async(&self.root, &self.root).await?;
                    self.dir_tokens.insert(token, self.root.clone());
                    Ok(())
                }
                EditorEvent::AddDir {
                    path,
                    child_token,
                    copy_from,
                    ..
                } => {
                    let dir = self.repo_path_to_fs(&path, true)?;
                    create_dir_all_no_symlink_async(&self.root, &dir).await?;
                    if let Some((src_path, _src_rev)) = copy_from {
                        let src = self.repo_path_to_fs(&src_path, true)?;
                        ensure_no_symlink_prefix_async(&self.root, &src).await?;
                        if tokio::fs::metadata(&src).await.is_ok() {
                            copy_dir_missing_recursive_async(&src, &dir).await?;
                        }
                    }
                    self.dir_tokens.insert(child_token, dir);
                    Ok(())
                }
                EditorEvent::OpenDir {
                    path, child_token, ..
                } => {
                    let dir = self.repo_path_to_fs(&path, true)?;
                    create_dir_all_no_symlink_async(&self.root, &dir).await?;
                    self.dir_tokens.insert(child_token, dir);
                    Ok(())
                }
                EditorEvent::CloseDir { dir_token } => {
                    let _ = self.dir_tokens.remove(&dir_token);
                    Ok(())
                }
                EditorEvent::DeleteEntry { path, .. } => {
                    let fs_path = self.repo_path_to_fs(&path, false)?;
                    if let Some(parent) = fs_path.parent() {
                        ensure_no_symlink_prefix_async(&self.root, parent).await?;
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
                        create_dir_all_no_symlink_async(&self.root, parent).await?;
                    }

                    if let Some((src_path, _src_rev)) = copy_from {
                        let src = self.repo_path_to_fs(&src_path, false)?;
                        ensure_no_symlink_prefix_async(&self.root, &src).await?;
                        self.file_copy_from.insert(file_token.clone(), src.clone());
                    }

                    self.file_tokens.insert(file_token, file_path);
                    Ok(())
                }
                EditorEvent::OpenFile {
                    path, file_token, ..
                } => {
                    let file_path = self.repo_path_to_fs(&path, false)?;
                    if let Some(parent) = file_path.parent() {
                        create_dir_all_no_symlink_async(&self.root, parent).await?;
                    }
                    self.file_tokens.insert(file_token, file_path);
                    Ok(())
                }
                EditorEvent::ApplyTextDelta { file_token, .. } => {
                    let dest = self.file_tokens.get(&file_token).cloned().ok_or_else(|| {
                        SvnError::Protocol("apply-textdelta for unknown file token".into())
                    })?;

                    ensure_no_symlink_prefix_async(&self.root, &dest).await?;

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
                            if let Some(src) = self.file_copy_from.get(&file_token) {
                                ensure_no_symlink_prefix_async(&self.root, src).await?;
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
                        ensure_no_symlink_prefix_async(&self.root, &pending.dest).await?;

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
                                let src = self.file_copy_from.get(&file_token);
                                match src {
                                    Some(src) => {
                                        ensure_no_symlink_prefix_async(&self.root, src).await?;
                                        if tokio::fs::metadata(src).await.is_ok() {
                                            ensure_no_symlink_prefix_async(&self.root, &dest)
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
                                    _ => {
                                        return Err(SvnError::Protocol(
                                            "close-file for missing file without textdelta".into(),
                                        ));
                                    }
                                }
                            }
                            Err(err) => return Err(err.into()),
                        }
                    }

                    let _ = self.file_tokens.remove(&file_token);
                    let _ = self.file_copy_from.remove(&file_token);
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
                    self.file_copy_from.clear();
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

fn is_symlink_like(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0
    }

    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

fn ensure_no_symlink_prefix(root: &Path, path: &Path) -> Result<(), SvnError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| SvnError::InvalidPath("unsafe path".into()))?;

    let mut cur = root.to_path_buf();
    for component in rel.components() {
        cur.push(component);

        match std::fs::symlink_metadata(&cur) {
            Ok(meta) => {
                if is_symlink_like(&meta) {
                    return Err(SvnError::InvalidPath(
                        "refusing to write through a symlink/reparse point".into(),
                    ));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        }
    }

    Ok(())
}

async fn ensure_no_symlink_prefix_async(root: &Path, path: &Path) -> Result<(), SvnError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| SvnError::InvalidPath("unsafe path".into()))?;

    let mut cur = root.to_path_buf();
    for component in rel.components() {
        cur.push(component);

        match tokio::fs::symlink_metadata(&cur).await {
            Ok(meta) => {
                if is_symlink_like(&meta) {
                    return Err(SvnError::InvalidPath(
                        "refusing to write through a symlink/reparse point".into(),
                    ));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(err.into()),
        }
    }

    Ok(())
}

fn create_dir_all_no_symlink(root: &Path, dir: &Path) -> Result<(), SvnError> {
    let rel = dir
        .strip_prefix(root)
        .map_err(|_| SvnError::InvalidPath("unsafe path".into()))?;

    std::fs::create_dir_all(root)?;

    let mut cur = root.to_path_buf();
    for component in rel.components() {
        cur.push(component);

        match std::fs::symlink_metadata(&cur) {
            Ok(meta) => {
                if is_symlink_like(&meta) {
                    return Err(SvnError::InvalidPath(
                        "refusing to write through a symlink/reparse point".into(),
                    ));
                }
                if !meta.is_dir() {
                    return Err(SvnError::InvalidPath(
                        "refusing to create a directory over a non-directory".into(),
                    ));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(&cur) {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(err.into()),
                }

                let meta = std::fs::symlink_metadata(&cur)?;
                if is_symlink_like(&meta) {
                    return Err(SvnError::InvalidPath(
                        "refusing to write through a symlink/reparse point".into(),
                    ));
                }
                if !meta.is_dir() {
                    return Err(SvnError::InvalidPath(
                        "refusing to create a directory over a non-directory".into(),
                    ));
                }
            }
            Err(err) => return Err(err.into()),
        }
    }

    Ok(())
}

async fn create_dir_all_no_symlink_async(root: &Path, dir: &Path) -> Result<(), SvnError> {
    let rel = dir
        .strip_prefix(root)
        .map_err(|_| SvnError::InvalidPath("unsafe path".into()))?;

    tokio::fs::create_dir_all(root).await?;

    let mut cur = root.to_path_buf();
    for component in rel.components() {
        cur.push(component);

        match tokio::fs::symlink_metadata(&cur).await {
            Ok(meta) => {
                if is_symlink_like(&meta) {
                    return Err(SvnError::InvalidPath(
                        "refusing to write through a symlink/reparse point".into(),
                    ));
                }
                if !meta.is_dir() {
                    return Err(SvnError::InvalidPath(
                        "refusing to create a directory over a non-directory".into(),
                    ));
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                match tokio::fs::create_dir(&cur).await {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(err) => return Err(err.into()),
                }

                let meta = tokio::fs::symlink_metadata(&cur).await?;
                if is_symlink_like(&meta) {
                    return Err(SvnError::InvalidPath(
                        "refusing to write through a symlink/reparse point".into(),
                    ));
                }
                if !meta.is_dir() {
                    return Err(SvnError::InvalidPath(
                        "refusing to create a directory over a non-directory".into(),
                    ));
                }
            }
            Err(err) => return Err(err.into()),
        }
    }

    Ok(())
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

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), SvnError> {
    if dest.starts_with(src) {
        return Err(SvnError::InvalidPath(
            "refusing to copy a directory into its own subtree".into(),
        ));
    }

    let mut stack = vec![(src.to_path_buf(), dest.to_path_buf())];
    while let Some((src_dir, dest_dir)) = stack.pop() {
        if let Ok(meta) = std::fs::symlink_metadata(&dest_dir)
            && is_symlink_like(&meta)
        {
            return Err(SvnError::InvalidPath(
                "refusing to copy into a symlink/reparse point".into(),
            ));
        }
        std::fs::create_dir_all(&dest_dir)?;
        for entry in std::fs::read_dir(&src_dir)? {
            let entry = entry?;
            let src_path = entry.path();
            let src_meta = std::fs::symlink_metadata(&src_path)?;
            if is_symlink_like(&src_meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a symlink/reparse point".into(),
                ));
            }
            let file_type = src_meta.file_type();
            let dest_path = dest_dir.join(entry.file_name());

            if file_type.is_dir() {
                stack.push((src_path, dest_path));
                continue;
            }

            if file_type.is_file() {
                if let Ok(meta) = std::fs::symlink_metadata(&dest_path)
                    && is_symlink_like(&meta)
                {
                    return Err(SvnError::InvalidPath(
                        "refusing to copy a file over a symlink/reparse point".into(),
                    ));
                }
                let _ = std::fs::copy(&src_path, &dest_path)?;
                continue;
            }

            return Err(SvnError::InvalidPath(
                "refusing to copy an unknown file type".into(),
            ));
        }
    }
    Ok(())
}

async fn copy_dir_recursive_async(src: &Path, dest: &Path) -> Result<(), SvnError> {
    if dest.starts_with(src) {
        return Err(SvnError::InvalidPath(
            "refusing to copy a directory into its own subtree".into(),
        ));
    }

    let mut stack = vec![(src.to_path_buf(), dest.to_path_buf())];
    while let Some((src_dir, dest_dir)) = stack.pop() {
        if let Ok(meta) = tokio::fs::symlink_metadata(&dest_dir).await
            && is_symlink_like(&meta)
        {
            return Err(SvnError::InvalidPath(
                "refusing to copy into a symlink/reparse point".into(),
            ));
        }
        tokio::fs::create_dir_all(&dest_dir).await?;
        let mut rd = tokio::fs::read_dir(&src_dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let src_path = entry.path();
            let src_meta = tokio::fs::symlink_metadata(&src_path).await?;
            if is_symlink_like(&src_meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a symlink/reparse point".into(),
                ));
            }
            let file_type = src_meta.file_type();
            let dest_path = dest_dir.join(entry.file_name());

            if file_type.is_dir() {
                stack.push((src_path, dest_path));
                continue;
            }

            if file_type.is_file() {
                if let Ok(meta) = tokio::fs::symlink_metadata(&dest_path).await
                    && is_symlink_like(&meta)
                {
                    return Err(SvnError::InvalidPath(
                        "refusing to copy a file over a symlink/reparse point".into(),
                    ));
                }
                let _ = tokio::fs::copy(&src_path, &dest_path).await?;
                continue;
            }

            return Err(SvnError::InvalidPath(
                "refusing to copy an unknown file type".into(),
            ));
        }
    }
    Ok(())
}

fn copy_dir_missing_recursive(src: &Path, dest: &Path) -> Result<(), SvnError> {
    if dest.starts_with(src) {
        return Err(SvnError::InvalidPath(
            "refusing to copy a directory into its own subtree".into(),
        ));
    }

    let mut stack = vec![(src.to_path_buf(), dest.to_path_buf())];
    while let Some((src_dir, dest_dir)) = stack.pop() {
        if let Ok(meta) = std::fs::symlink_metadata(&dest_dir)
            && is_symlink_like(&meta)
        {
            return Err(SvnError::InvalidPath(
                "refusing to copy into a symlink/reparse point".into(),
            ));
        }
        std::fs::create_dir_all(&dest_dir)?;
        for entry in std::fs::read_dir(&src_dir)? {
            let entry = entry?;
            let src_path = entry.path();
            let src_meta = std::fs::symlink_metadata(&src_path)?;
            if is_symlink_like(&src_meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a symlink/reparse point".into(),
                ));
            }
            let file_type = src_meta.file_type();
            let dest_path = dest_dir.join(entry.file_name());

            if let Ok(meta) = std::fs::symlink_metadata(&dest_path)
                && is_symlink_like(&meta)
            {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a file over a symlink/reparse point".into(),
                ));
            }

            if dest_path.exists() {
                if file_type.is_dir()
                    && let Ok(dest_meta) = std::fs::metadata(&dest_path)
                    && dest_meta.is_dir()
                {
                    stack.push((src_path, dest_path));
                }
                continue;
            }

            if file_type.is_dir() {
                copy_dir_recursive(&src_path, &dest_path)?;
                continue;
            }

            if file_type.is_file() {
                let _ = std::fs::copy(&src_path, &dest_path)?;
                continue;
            }

            return Err(SvnError::InvalidPath(
                "refusing to copy an unknown file type".into(),
            ));
        }
    }
    Ok(())
}

async fn copy_dir_missing_recursive_async(src: &Path, dest: &Path) -> Result<(), SvnError> {
    if dest.starts_with(src) {
        return Err(SvnError::InvalidPath(
            "refusing to copy a directory into its own subtree".into(),
        ));
    }

    let mut stack = vec![(src.to_path_buf(), dest.to_path_buf())];
    while let Some((src_dir, dest_dir)) = stack.pop() {
        if let Ok(meta) = tokio::fs::symlink_metadata(&dest_dir).await
            && is_symlink_like(&meta)
        {
            return Err(SvnError::InvalidPath(
                "refusing to copy into a symlink/reparse point".into(),
            ));
        }
        tokio::fs::create_dir_all(&dest_dir).await?;
        let mut rd = tokio::fs::read_dir(&src_dir).await?;
        while let Some(entry) = rd.next_entry().await? {
            let src_path = entry.path();
            let src_meta = tokio::fs::symlink_metadata(&src_path).await?;
            if is_symlink_like(&src_meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a symlink/reparse point".into(),
                ));
            }
            let file_type = src_meta.file_type();
            let dest_path = dest_dir.join(entry.file_name());

            if let Ok(meta) = tokio::fs::symlink_metadata(&dest_path).await
                && is_symlink_like(&meta)
            {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a file over a symlink/reparse point".into(),
                ));
            }

            if tokio::fs::metadata(&dest_path).await.is_ok() {
                if file_type.is_dir()
                    && tokio::fs::metadata(&dest_path)
                        .await
                        .is_ok_and(|m| m.is_dir())
                {
                    stack.push((src_path, dest_path));
                }
                continue;
            }

            if file_type.is_dir() {
                copy_dir_recursive_async(&src_path, &dest_path).await?;
                continue;
            }

            if file_type.is_file() {
                let _ = tokio::fs::copy(&src_path, &dest_path).await?;
                continue;
            }

            return Err(SvnError::InvalidPath(
                "refusing to copy an unknown file type".into(),
            ));
        }
    }
    Ok(())
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

    #[cfg(unix)]
    #[test]
    fn fs_editor_rejects_symlink_parent_dir() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let outside = tempfile::tempdir().unwrap();

        symlink(outside.path(), root.join("trunk")).unwrap();

        let mut editor = FsEditor::new(root);
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .unwrap();

        let err = editor
            .on_event(EditorEvent::AddFile {
                path: "trunk/hello.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .unwrap_err();
        assert!(matches!(err, SvnError::InvalidPath(_)));
    }

    #[cfg(unix)]
    #[test]
    fn tokio_fs_editor_rejects_symlink_parent_dir() {
        use std::os::unix::fs::symlink;

        run_async(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();
            let outside = tempfile::tempdir().unwrap();

            symlink(outside.path(), root.join("trunk")).unwrap();

            let mut editor = TokioFsEditor::new(root);
            editor
                .on_event(EditorEvent::OpenRoot {
                    rev: None,
                    token: "d0".to_string(),
                })
                .await
                .unwrap();

            let err = editor
                .on_event(EditorEvent::AddFile {
                    path: "trunk/hello.txt".to_string(),
                    dir_token: "d0".to_string(),
                    file_token: "f1".to_string(),
                    copy_from: None,
                })
                .await
                .unwrap_err();
            assert!(matches!(err, SvnError::InvalidPath(_)));
        });
    }

    #[cfg(unix)]
    #[test]
    fn fs_editor_delete_entry_removes_symlink_without_following_target() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel.txt");
        std::fs::write(&sentinel, b"sentinel").unwrap();

        symlink(outside.path(), root.join("trunk")).unwrap();

        let mut editor = FsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .unwrap();

        editor
            .on_event(EditorEvent::DeleteEntry {
                path: "trunk".to_string(),
                rev: 1,
                dir_token: "d0".to_string(),
            })
            .unwrap();

        assert!(!root.join("trunk").exists());
        assert!(sentinel.exists());
    }

    #[cfg(unix)]
    #[test]
    fn tokio_fs_editor_delete_entry_removes_symlink_without_following_target() {
        use std::os::unix::fs::symlink;

        run_async(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();
            let outside = tempfile::tempdir().unwrap();
            let sentinel = outside.path().join("sentinel.txt");
            std::fs::write(&sentinel, b"sentinel").unwrap();

            symlink(outside.path(), root.join("trunk")).unwrap();

            let mut editor = TokioFsEditor::new(root.clone());
            editor
                .on_event(EditorEvent::OpenRoot {
                    rev: None,
                    token: "d0".to_string(),
                })
                .await
                .unwrap();

            editor
                .on_event(EditorEvent::DeleteEntry {
                    path: "trunk".to_string(),
                    rev: 1,
                    dir_token: "d0".to_string(),
                })
                .await
                .unwrap();

            assert!(!root.join("trunk").exists());
            assert!(sentinel.exists());
        });
    }

    #[cfg(windows)]
    fn try_create_junction(link: &Path, target: &Path) -> bool {
        use std::process::Command;

        let cmd = format!("mklink /J \"{}\" \"{}\"", link.display(), target.display());
        let Ok(out) = Command::new("cmd").args(["/C", &cmd]).output() else {
            return false;
        };
        out.status.success()
    }

    #[cfg(windows)]
    #[test]
    fn fs_editor_rejects_junction_parent_dir() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let outside = tempfile::tempdir().unwrap();

        if !try_create_junction(&root.join("trunk"), outside.path()) {
            return;
        }

        let mut editor = FsEditor::new(root);
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .unwrap();

        let err = editor
            .on_event(EditorEvent::AddFile {
                path: "trunk/hello.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: None,
            })
            .unwrap_err();
        assert!(matches!(err, SvnError::InvalidPath(_)));
    }

    #[cfg(windows)]
    #[test]
    fn tokio_fs_editor_rejects_junction_parent_dir() {
        run_async(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();
            let outside = tempfile::tempdir().unwrap();

            if !try_create_junction(&root.join("trunk"), outside.path()) {
                return;
            }

            let mut editor = TokioFsEditor::new(root);
            editor
                .on_event(EditorEvent::OpenRoot {
                    rev: None,
                    token: "d0".to_string(),
                })
                .await
                .unwrap();

            let err = editor
                .on_event(EditorEvent::AddFile {
                    path: "trunk/hello.txt".to_string(),
                    dir_token: "d0".to_string(),
                    file_token: "f1".to_string(),
                    copy_from: None,
                })
                .await
                .unwrap_err();
            assert!(matches!(err, SvnError::InvalidPath(_)));
        });
    }

    #[cfg(windows)]
    #[test]
    fn fs_editor_delete_entry_removes_junction_without_following_target() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("sentinel.txt");
        std::fs::write(&sentinel, b"sentinel").unwrap();

        if !try_create_junction(&root.join("trunk"), outside.path()) {
            return;
        }

        let mut editor = FsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .unwrap();

        editor
            .on_event(EditorEvent::DeleteEntry {
                path: "trunk".to_string(),
                rev: 1,
                dir_token: "d0".to_string(),
            })
            .unwrap();

        assert!(!root.join("trunk").exists());
        assert!(sentinel.exists());
    }

    #[cfg(windows)]
    #[test]
    fn tokio_fs_editor_delete_entry_removes_junction_without_following_target() {
        run_async(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();
            let outside = tempfile::tempdir().unwrap();
            let sentinel = outside.path().join("sentinel.txt");
            std::fs::write(&sentinel, b"sentinel").unwrap();

            if !try_create_junction(&root.join("trunk"), outside.path()) {
                return;
            }

            let mut editor = TokioFsEditor::new(root.clone());
            editor
                .on_event(EditorEvent::OpenRoot {
                    rev: None,
                    token: "d0".to_string(),
                })
                .await
                .unwrap();

            editor
                .on_event(EditorEvent::DeleteEntry {
                    path: "trunk".to_string(),
                    rev: 1,
                    dir_token: "d0".to_string(),
                })
                .await
                .unwrap();

            assert!(!root.join("trunk").exists());
            assert!(sentinel.exists());
        });
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

    #[test]
    fn fs_editor_copies_file_from_copyfrom_when_no_textdelta_is_sent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        std::fs::write(root.join("src.txt"), b"hello").unwrap();

        let mut editor = FsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .unwrap();
        editor
            .on_event(EditorEvent::AddFile {
                path: "dst.txt".to_string(),
                dir_token: "d0".to_string(),
                file_token: "f1".to_string(),
                copy_from: Some(("src.txt".to_string(), 1)),
            })
            .unwrap();
        editor
            .on_event(EditorEvent::CloseFile {
                file_token: "f1".to_string(),
                text_checksum: None,
            })
            .unwrap();
        editor.on_event(EditorEvent::CloseEdit).unwrap();

        assert_eq!(std::fs::read(root.join("dst.txt")).unwrap(), b"hello");
    }

    #[test]
    fn fs_editor_copies_dir_from_copyfrom() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        std::fs::create_dir_all(root.join("srcdir/sub")).unwrap();
        std::fs::write(root.join("srcdir/sub/file.txt"), b"hello").unwrap();

        let mut editor = FsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .unwrap();
        editor
            .on_event(EditorEvent::AddDir {
                path: "destdir".to_string(),
                parent_token: "d0".to_string(),
                child_token: "d1".to_string(),
                copy_from: Some(("srcdir".to_string(), 1)),
            })
            .unwrap();
        editor
            .on_event(EditorEvent::CloseDir {
                dir_token: "d1".to_string(),
            })
            .unwrap();
        editor.on_event(EditorEvent::CloseEdit).unwrap();

        assert_eq!(
            std::fs::read(root.join("destdir/sub/file.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn fs_editor_dir_copyfrom_provides_base_for_identity_textdelta() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().to_path_buf();

        std::fs::create_dir_all(root.join("srcdir/sub")).unwrap();
        std::fs::write(root.join("srcdir/sub/file.txt"), b"hello").unwrap();

        let mut editor = FsEditor::new(root.clone());
        editor
            .on_event(EditorEvent::OpenRoot {
                rev: None,
                token: "d0".to_string(),
            })
            .unwrap();
        editor
            .on_event(EditorEvent::AddDir {
                path: "destdir".to_string(),
                parent_token: "d0".to_string(),
                child_token: "d1".to_string(),
                copy_from: Some(("srcdir".to_string(), 1)),
            })
            .unwrap();
        editor
            .on_event(EditorEvent::OpenFile {
                path: "destdir/sub/file.txt".to_string(),
                dir_token: "d1".to_string(),
                file_token: "f1".to_string(),
                rev: 1,
            })
            .unwrap();
        editor
            .on_event(EditorEvent::ApplyTextDelta {
                file_token: "f1".to_string(),
                base_checksum: None,
            })
            .unwrap();
        editor
            .on_event(EditorEvent::TextDeltaEnd {
                file_token: "f1".to_string(),
            })
            .unwrap();
        editor
            .on_event(EditorEvent::CloseFile {
                file_token: "f1".to_string(),
                text_checksum: None,
            })
            .unwrap();
        editor
            .on_event(EditorEvent::CloseDir {
                dir_token: "d1".to_string(),
            })
            .unwrap();
        editor.on_event(EditorEvent::CloseEdit).unwrap();

        assert_eq!(
            std::fs::read(root.join("destdir/sub/file.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn tokio_fs_editor_copies_file_from_copyfrom_when_no_textdelta_is_sent() {
        run_async(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();

            std::fs::write(root.join("src.txt"), b"hello").unwrap();

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
                    path: "dst.txt".to_string(),
                    dir_token: "d0".to_string(),
                    file_token: "f1".to_string(),
                    copy_from: Some(("src.txt".to_string(), 1)),
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

            let written = tokio::fs::read(root.join("dst.txt")).await.unwrap();
            assert_eq!(written, b"hello");
        });
    }

    #[test]
    fn tokio_fs_editor_copies_dir_from_copyfrom() {
        run_async(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();

            std::fs::create_dir_all(root.join("srcdir/sub")).unwrap();
            std::fs::write(root.join("srcdir/sub/file.txt"), b"hello").unwrap();

            let mut editor = TokioFsEditor::new(root.clone());
            editor
                .on_event(EditorEvent::OpenRoot {
                    rev: None,
                    token: "d0".to_string(),
                })
                .await
                .unwrap();
            editor
                .on_event(EditorEvent::AddDir {
                    path: "destdir".to_string(),
                    parent_token: "d0".to_string(),
                    child_token: "d1".to_string(),
                    copy_from: Some(("srcdir".to_string(), 1)),
                })
                .await
                .unwrap();
            editor
                .on_event(EditorEvent::CloseDir {
                    dir_token: "d1".to_string(),
                })
                .await
                .unwrap();
            editor.on_event(EditorEvent::CloseEdit).await.unwrap();

            let written = tokio::fs::read(root.join("destdir/sub/file.txt"))
                .await
                .unwrap();
            assert_eq!(written, b"hello");
        });
    }

    #[test]
    fn tokio_fs_editor_dir_copyfrom_provides_base_for_identity_textdelta() {
        run_async(async {
            let temp = tempfile::tempdir().unwrap();
            let root = temp.path().to_path_buf();

            std::fs::create_dir_all(root.join("srcdir/sub")).unwrap();
            std::fs::write(root.join("srcdir/sub/file.txt"), b"hello").unwrap();

            let mut editor = TokioFsEditor::new(root.clone());
            editor
                .on_event(EditorEvent::OpenRoot {
                    rev: None,
                    token: "d0".to_string(),
                })
                .await
                .unwrap();
            editor
                .on_event(EditorEvent::AddDir {
                    path: "destdir".to_string(),
                    parent_token: "d0".to_string(),
                    child_token: "d1".to_string(),
                    copy_from: Some(("srcdir".to_string(), 1)),
                })
                .await
                .unwrap();
            editor
                .on_event(EditorEvent::OpenFile {
                    path: "destdir/sub/file.txt".to_string(),
                    dir_token: "d1".to_string(),
                    file_token: "f1".to_string(),
                    rev: 1,
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
            editor
                .on_event(EditorEvent::CloseDir {
                    dir_token: "d1".to_string(),
                })
                .await
                .unwrap();
            editor.on_event(EditorEvent::CloseEdit).await.unwrap();

            let written = tokio::fs::read(root.join("destdir/sub/file.txt"))
                .await
                .unwrap();
            assert_eq!(written, b"hello");
        });
    }
}
