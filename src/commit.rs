use std::collections::{BTreeMap, BTreeSet};

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::path::{validate_rel_dir_path, validate_rel_path};
use crate::svndiff::{SvndiffVersion, encode_fulltext_with_options, encode_insertion_window};
use crate::{
    Capability, CommitInfo, CommitOptions, EditorCommand, NodeKind, RaSvnSession, SvnError,
};

/// Svndiff version selection for [`CommitBuilder`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SvndiffMode {
    /// Select the best supported version for the current server.
    #[default]
    Auto,
    /// Emit svndiff0 (no secondary compression).
    V0,
    /// Emit svndiff1 (zlib-compressed sections).
    V1,
    /// Emit svndiff2 (LZ4-compressed sections).
    V2,
}

/// High-level commit editor builder.
///
/// This builder generates a low-level editor command sequence so callers don't
/// need to manually craft `EditorCommand::TextDeltaChunk` values.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct CommitBuilder {
    base_rev: Option<u64>,
    svndiff: SvndiffMode,
    zlib_level: u32,
    window_size: usize,
    changes: Vec<Change>,
}

/// High-level commit builder that streams file contents from an [`AsyncRead`].
///
/// This is a specialized helper for committing large files without buffering
/// the full contents in memory.
pub struct CommitStreamBuilder {
    base_rev: Option<u64>,
    svndiff: SvndiffMode,
    zlib_level: u32,
    window_size: usize,
    files: Vec<StreamFileChange>,
}

struct StreamFileChange {
    path: String,
    reader: Box<dyn AsyncRead + Unpin>,
}

impl CommitStreamBuilder {
    /// Creates an empty streaming commit builder.
    pub fn new() -> Self {
        Self {
            base_rev: None,
            svndiff: SvndiffMode::Auto,
            zlib_level: 5,
            window_size: 64 * 1024,
            files: Vec::new(),
        }
    }

    /// Sets the base revision used for `open-root` and `open-file`.
    ///
    /// If not set, [`CommitStreamBuilder::commit`] will query the server for `HEAD`
    /// via `get-latest-rev`.
    pub fn with_base_rev(mut self, base_rev: u64) -> Self {
        self.base_rev = Some(base_rev);
        self
    }

    /// Sets the svndiff version to use.
    pub fn with_svndiff(mut self, svndiff: SvndiffMode) -> Self {
        self.svndiff = svndiff;
        self
    }

    /// Sets the zlib compression level used by svndiff1.
    ///
    /// Valid values are `0..=9`. `0` disables compression and sends raw data
    /// with an svndiff1 size prefix (matching Subversion behavior).
    pub fn with_zlib_level(mut self, level: u32) -> Self {
        self.zlib_level = level;
        self
    }

    /// Sets the maximum data size per svndiff window.
    pub fn with_window_size(mut self, window_size: usize) -> Self {
        self.window_size = window_size;
        self
    }

    /// Adds or replaces the full contents of `path` from `reader`.
    pub fn put_file_reader<R>(mut self, path: impl Into<String>, reader: R) -> Self
    where
        R: AsyncRead + Unpin + 'static,
    {
        self.files.push(StreamFileChange {
            path: path.into(),
            reader: Box::new(reader),
        });
        self
    }

    /// Commits the streamed edit to `session`.
    pub async fn commit(
        mut self,
        session: &mut RaSvnSession,
        options: &CommitOptions,
    ) -> Result<CommitInfo, SvnError> {
        if self.files.is_empty() {
            return Err(SvnError::Protocol("commit has no changes".into()));
        }
        if self.zlib_level > 9 {
            return Err(SvnError::Protocol("zlib level must be 0..=9".into()));
        }

        let base_rev = match self.base_rev {
            Some(rev) => rev,
            None => session.get_latest_rev().await?,
        };

        let svndiff_version = match self.svndiff {
            SvndiffMode::Auto => select_svndiff_version(session),
            SvndiffMode::V0 => SvndiffVersion::V0,
            SvndiffMode::V1 => {
                if !session.has_capability(Capability::Svndiff1) {
                    return Err(SvnError::Protocol(
                        "server does not support svndiff1".into(),
                    ));
                }
                SvndiffVersion::V1
            }
            SvndiffMode::V2 => {
                if !session.has_capability(Capability::AcceptsSvndiff2) {
                    return Err(SvnError::Protocol(
                        "server does not support svndiff2".into(),
                    ));
                }
                SvndiffVersion::V2
            }
        };

        // Validate paths and compute which directories to create vs open.
        let mut files = Vec::<StreamResolvedFile>::new();
        let mut required_dirs = BTreeSet::<String>::new();
        for file in self.files.drain(..) {
            let path = validate_rel_path(&file.path)?;
            let parent = parent_dir(&path);
            for dir in dir_prefixes(&parent) {
                required_dirs.insert(dir);
            }
            let kind = session.check_path(&path, Some(base_rev)).await?;
            match kind {
                NodeKind::None | NodeKind::File => {}
                NodeKind::Dir | NodeKind::Unknown => {
                    return Err(SvnError::Protocol(format!(
                        "expected file or none at {path} (got {kind})"
                    )));
                }
            }
            files.push(StreamResolvedFile {
                path,
                exists: kind == NodeKind::File,
                reader: file.reader,
            });
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));

        let mut dir_plans = BTreeMap::<String, DirPlanKind>::new();
        for dir in required_dirs {
            let dir = validate_rel_dir_path(&dir)?;
            let kind = session.check_path(&dir, Some(base_rev)).await?;
            match kind {
                NodeKind::Dir => {
                    dir_plans.insert(dir, DirPlanKind::Open);
                }
                NodeKind::None => {
                    dir_plans.insert(dir, DirPlanKind::Add { copy_from: None });
                }
                NodeKind::File | NodeKind::Unknown => {
                    return Err(SvnError::Protocol(format!(
                        "expected directory or none at {dir} (got {kind})"
                    )));
                }
            }
        }

        session
            .commit_drive(options, move |drive| {
                Box::pin(async move {
                    let mut token_gen = TokenGen::default();
                    let root_token = "r".to_string();
                    let mut stack: Vec<(String, String)> =
                        vec![(String::new(), root_token.clone())];

                    drive
                        .send(&EditorCommand::OpenRoot {
                            rev: Some(base_rev),
                            token: root_token.clone(),
                        })
                        .await?;

                    let window_size = self.window_size.max(1);

                    for mut file in files {
                        let parent = parent_dir(&file.path);
                        let target_dirs = dir_prefixes(&parent);

                        let mut lcp = 0usize;
                        while lcp < target_dirs.len()
                            && lcp + 1 < stack.len()
                            && stack[lcp + 1].0 == target_dirs[lcp]
                        {
                            lcp += 1;
                        }

                        while stack.len() > lcp + 1 {
                            let (_, token) = stack.pop().ok_or_else(|| {
                                SvnError::Protocol("commit dir stack underflow".into())
                            })?;
                            drive
                                .send(&EditorCommand::CloseDir { dir_token: token })
                                .await?;
                        }

                        for dir_path in &target_dirs[lcp..] {
                            let parent_token = stack
                                .last()
                                .map(|(_, token)| token.clone())
                                .ok_or_else(|| {
                                    SvnError::Protocol("missing parent dir token".into())
                                })?;
                            let token = token_gen.dir();
                            let plan = dir_plans.get(dir_path).ok_or_else(|| {
                                SvnError::Protocol(format!(
                                    "missing directory plan for '{dir_path}'"
                                ))
                            })?;
                            match plan {
                                DirPlanKind::Open => {
                                    drive
                                        .send(&EditorCommand::OpenDir {
                                            path: dir_path.clone(),
                                            parent_token,
                                            child_token: token.clone(),
                                            rev: base_rev,
                                        })
                                        .await?;
                                }
                                DirPlanKind::Add { copy_from } => {
                                    drive
                                        .send(&EditorCommand::AddDir {
                                            path: dir_path.clone(),
                                            parent_token,
                                            child_token: token.clone(),
                                            copy_from: copy_from.clone(),
                                        })
                                        .await?;
                                }
                            }
                            stack.push((dir_path.clone(), token));
                        }

                        let dir_token =
                            stack
                                .last()
                                .map(|(_, token)| token.clone())
                                .ok_or_else(|| {
                                    SvnError::Protocol("missing current dir token".into())
                                })?;
                        let file_token = token_gen.file();

                        if file.exists {
                            drive
                                .send(&EditorCommand::OpenFile {
                                    path: file.path.clone(),
                                    dir_token,
                                    file_token: file_token.clone(),
                                    rev: base_rev,
                                })
                                .await?;
                        } else {
                            drive
                                .send(&EditorCommand::AddFile {
                                    path: file.path.clone(),
                                    dir_token,
                                    file_token: file_token.clone(),
                                    copy_from: None,
                                })
                                .await?;
                        }

                        drive
                            .send(&EditorCommand::ApplyTextDelta {
                                file_token: file_token.clone(),
                                base_checksum: None,
                            })
                            .await?;

                        let mut buf = vec![0u8; window_size];
                        let mut any = false;
                        let mut first_window = true;
                        loop {
                            let n = file.reader.read(&mut buf).await?;
                            if n == 0 {
                                break;
                            }
                            any = true;

                            let mut delta = Vec::new();
                            if first_window {
                                delta.extend_from_slice(&svndiff_version.header());
                                first_window = false;
                            }
                            encode_insertion_window(
                                svndiff_version,
                                &buf[..n],
                                self.zlib_level,
                                &mut delta,
                            )?;
                            for chunk in delta.chunks(64 * 1024) {
                                drive
                                    .send(&EditorCommand::TextDeltaChunk {
                                        file_token: file_token.clone(),
                                        chunk: chunk.to_vec(),
                                    })
                                    .await?;
                            }
                        }

                        if !any {
                            let mut delta = Vec::new();
                            delta.extend_from_slice(&svndiff_version.header());
                            encode_insertion_window(
                                svndiff_version,
                                &[],
                                self.zlib_level,
                                &mut delta,
                            )?;
                            for chunk in delta.chunks(64 * 1024) {
                                drive
                                    .send(&EditorCommand::TextDeltaChunk {
                                        file_token: file_token.clone(),
                                        chunk: chunk.to_vec(),
                                    })
                                    .await?;
                            }
                        }

                        drive
                            .send(&EditorCommand::TextDeltaEnd {
                                file_token: file_token.clone(),
                            })
                            .await?;
                        drive
                            .send(&EditorCommand::CloseFile {
                                file_token,
                                text_checksum: None,
                            })
                            .await?;
                    }

                    while stack.len() > 1 {
                        let (_, token) = stack.pop().ok_or_else(|| {
                            SvnError::Protocol("commit dir stack underflow".into())
                        })?;
                        drive
                            .send(&EditorCommand::CloseDir { dir_token: token })
                            .await?;
                    }

                    drive
                        .send(&EditorCommand::CloseDir {
                            dir_token: root_token,
                        })
                        .await?;
                    drive.send(&EditorCommand::CloseEdit).await?;
                    Ok(())
                })
            })
            .await
    }
}

impl Default for CommitStreamBuilder {
    fn default() -> Self {
        Self::new()
    }
}

struct StreamResolvedFile {
    path: String,
    exists: bool,
    reader: Box<dyn AsyncRead + Unpin>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
enum Change {
    PutFile {
        path: String,
        contents: Vec<u8>,
    },
    MkdirP {
        path: String,
    },
    Delete {
        path: String,
    },
    Copy {
        from_path: String,
        from_rev: Option<u64>,
        to_path: String,
    },
    FileProp {
        path: String,
        name: String,
        value: Option<Vec<u8>>,
    },
    DirProp {
        path: String,
        name: String,
        value: Option<Vec<u8>>,
    },
}

impl CommitBuilder {
    /// Creates an empty commit builder.
    pub fn new() -> Self {
        Self {
            base_rev: None,
            svndiff: SvndiffMode::Auto,
            zlib_level: 5,
            window_size: 64 * 1024,
            changes: Vec::new(),
        }
    }

    /// Sets the base revision used for `open-root` and `open-file`.
    ///
    /// If not set, [`CommitBuilder::build_editor_commands`] will query the
    /// server for `HEAD` via `get-latest-rev`.
    pub fn with_base_rev(mut self, base_rev: u64) -> Self {
        self.base_rev = Some(base_rev);
        self
    }

    /// Sets the svndiff version to use.
    pub fn with_svndiff(mut self, svndiff: SvndiffMode) -> Self {
        self.svndiff = svndiff;
        self
    }

    /// Sets the zlib compression level used by svndiff1.
    ///
    /// Valid values are `0..=9`. `0` disables compression and sends raw data
    /// with an svndiff1 size prefix (matching Subversion behavior).
    pub fn with_zlib_level(mut self, level: u32) -> Self {
        self.zlib_level = level;
        self
    }

    /// Sets the maximum data size per svndiff window.
    pub fn with_window_size(mut self, window_size: usize) -> Self {
        self.window_size = window_size;
        self
    }

    /// Adds or replaces the full contents of `path`.
    ///
    /// If the path does not exist at `base_rev`, it will be added. If it exists
    /// as a file, it will be replaced via a textdelta. Directory paths are
    /// rejected.
    pub fn put_file(mut self, path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        self.changes.push(Change::PutFile {
            path: path.into(),
            contents: contents.into(),
        });
        self
    }

    /// Ensures `path` exists as a directory, creating parent directories as needed.
    ///
    /// If the directory already exists at `base_rev`, this is a no-op.
    pub fn mkdir_p(mut self, path: impl Into<String>) -> Self {
        self.changes.push(Change::MkdirP { path: path.into() });
        self
    }

    /// Deletes `path` (file or directory).
    pub fn delete(mut self, path: impl Into<String>) -> Self {
        self.changes.push(Change::Delete { path: path.into() });
        self
    }

    /// Copies `from_path@base_rev` to `to_path`.
    pub fn copy(mut self, from_path: impl Into<String>, to_path: impl Into<String>) -> Self {
        self.changes.push(Change::Copy {
            from_path: from_path.into(),
            from_rev: None,
            to_path: to_path.into(),
        });
        self
    }

    /// Copies `from_path@from_rev` to `to_path`.
    pub fn copy_from_rev(
        mut self,
        from_path: impl Into<String>,
        from_rev: u64,
        to_path: impl Into<String>,
    ) -> Self {
        self.changes.push(Change::Copy {
            from_path: from_path.into(),
            from_rev: Some(from_rev),
            to_path: to_path.into(),
        });
        self
    }

    /// Moves `from_path@base_rev` to `to_path`.
    ///
    /// This is expressed as `copy` + `delete`.
    pub fn move_path(self, from_path: impl Into<String>, to_path: impl Into<String>) -> Self {
        let from_path = from_path.into();
        self.copy(from_path.clone(), to_path).delete(from_path)
    }

    /// Moves `from_path@from_rev` to `to_path`.
    ///
    /// This is expressed as `copy_from_rev` + `delete`.
    pub fn move_path_from_rev(
        self,
        from_path: impl Into<String>,
        from_rev: u64,
        to_path: impl Into<String>,
    ) -> Self {
        let from_path = from_path.into();
        self.copy_from_rev(from_path.clone(), from_rev, to_path)
            .delete(from_path)
    }

    /// Sets or deletes a file property.
    pub fn file_prop(
        mut self,
        path: impl Into<String>,
        name: impl Into<String>,
        value: Option<Vec<u8>>,
    ) -> Self {
        self.changes.push(Change::FileProp {
            path: path.into(),
            name: name.into(),
            value,
        });
        self
    }

    /// Sets a file property.
    pub fn set_file_prop(
        self,
        path: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        self.file_prop(path, name, Some(value.into()))
    }

    /// Deletes a file property.
    pub fn delete_file_prop(self, path: impl Into<String>, name: impl Into<String>) -> Self {
        self.file_prop(path, name, None)
    }

    /// Sets or deletes a directory property.
    pub fn dir_prop(
        mut self,
        path: impl Into<String>,
        name: impl Into<String>,
        value: Option<Vec<u8>>,
    ) -> Self {
        self.changes.push(Change::DirProp {
            path: path.into(),
            name: name.into(),
            value,
        });
        self
    }

    /// Sets a directory property.
    pub fn set_dir_prop(
        self,
        path: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        self.dir_prop(path, name, Some(value.into()))
    }

    /// Deletes a directory property.
    pub fn delete_dir_prop(self, path: impl Into<String>, name: impl Into<String>) -> Self {
        self.dir_prop(path, name, None)
    }

    /// Builds a low-level editor command sequence suitable for
    /// [`RaSvnSession::commit`].
    pub async fn build_editor_commands(
        &self,
        session: &mut RaSvnSession,
    ) -> Result<Vec<EditorCommand>, SvnError> {
        if self.changes.is_empty() {
            return Err(SvnError::Protocol("commit has no changes".into()));
        }
        if self.zlib_level > 9 {
            return Err(SvnError::Protocol("zlib level must be 0..=9".into()));
        }

        let base_rev = match self.base_rev {
            Some(rev) => rev,
            None => session.get_latest_rev().await?,
        };

        let svndiff_version = match self.svndiff {
            SvndiffMode::Auto => select_svndiff_version(session),
            SvndiffMode::V0 => SvndiffVersion::V0,
            SvndiffMode::V1 => {
                if !session.has_capability(Capability::Svndiff1) {
                    return Err(SvnError::Protocol(
                        "server does not support svndiff1".into(),
                    ));
                }
                SvndiffVersion::V1
            }
            SvndiffMode::V2 => {
                if !session.has_capability(Capability::AcceptsSvndiff2) {
                    return Err(SvnError::Protocol(
                        "server does not support svndiff2".into(),
                    ));
                }
                SvndiffVersion::V2
            }
        };

        let mut file_ops = BTreeMap::<String, FileOp>::new();
        let mut dir_ops = BTreeMap::<String, DirOp>::new();
        let mut delete_paths = BTreeSet::<String>::new();
        let mut copy_ops = Vec::<CopyOp>::new();

        for change in &self.changes {
            match change {
                Change::PutFile { path, contents } => {
                    let path = validate_rel_path(path)?;
                    if delete_paths.contains(&path) {
                        return Err(SvnError::Protocol(format!(
                            "put-file conflicts with delete at {path}"
                        )));
                    }
                    let op = file_ops.entry(path).or_default();
                    if op.action.is_some() {
                        return Err(SvnError::Protocol(
                            "commit builder has multiple content actions for the same file".into(),
                        ));
                    }
                    op.action = Some(FileAction::Put(contents.clone()));
                }
                Change::MkdirP { path } => {
                    let path = validate_rel_dir_path(path)?;
                    if !path.is_empty() && delete_paths.contains(&path) {
                        return Err(SvnError::Protocol(format!(
                            "mkdir-p conflicts with delete at {path}"
                        )));
                    }
                    let op = dir_ops.entry(path).or_default();
                    if matches!(op.action.as_ref(), Some(DirAction::Copy { .. })) {
                        return Err(SvnError::Protocol(
                            "cannot combine mkdir-p with copy".into(),
                        ));
                    }
                    op.action = Some(DirAction::Mkdir);
                }
                Change::Delete { path } => {
                    let path = validate_rel_path(path)?;
                    delete_paths.insert(path);
                }
                Change::Copy {
                    from_path,
                    from_rev,
                    to_path,
                } => {
                    let from_path = validate_rel_path(from_path)?;
                    let to_path = validate_rel_path(to_path)?;
                    copy_ops.push(CopyOp {
                        from_path,
                        from_rev: *from_rev,
                        to_path,
                    });
                }
                Change::FileProp { path, name, value } => {
                    let path = validate_rel_path(path)?;
                    if delete_paths.contains(&path) {
                        return Err(SvnError::Protocol(format!(
                            "file-prop conflicts with delete at {path}"
                        )));
                    }
                    let op = file_ops.entry(path).or_default();
                    op.props.push(PropChange {
                        name: name.clone(),
                        value: value.clone(),
                    });
                }
                Change::DirProp { path, name, value } => {
                    let path = validate_rel_dir_path(path)?;
                    if !path.is_empty() && delete_paths.contains(&path) {
                        return Err(SvnError::Protocol(format!(
                            "dir-prop conflicts with delete at {path}"
                        )));
                    }
                    let op = dir_ops.entry(path).or_default();
                    op.props.push(PropChange {
                        name: name.clone(),
                        value: value.clone(),
                    });
                }
            }
        }

        // Resolve copy operations to file vs directory targets.
        for copy in copy_ops {
            if delete_paths.contains(&copy.to_path) {
                return Err(SvnError::Protocol(format!(
                    "copy destination conflicts with delete at {}",
                    copy.to_path
                )));
            }

            let from_rev = copy.from_rev.unwrap_or(base_rev);
            let kind = session.check_path(&copy.from_path, Some(from_rev)).await?;
            match kind {
                NodeKind::File => {
                    let op = file_ops.entry(copy.to_path).or_default();
                    if op.action.is_some() {
                        return Err(SvnError::Protocol(
                            "commit builder has multiple content actions for the same file".into(),
                        ));
                    }
                    op.action = Some(FileAction::Copy {
                        from_path: copy.from_path,
                        from_rev,
                    });
                }
                NodeKind::Dir => {
                    let op = dir_ops.entry(copy.to_path).or_default();
                    if matches!(op.action.as_ref(), Some(DirAction::Mkdir)) {
                        return Err(SvnError::Protocol(
                            "cannot combine copy with mkdir-p".into(),
                        ));
                    }
                    if matches!(op.action.as_ref(), Some(DirAction::Copy { .. })) {
                        return Err(SvnError::Protocol(
                            "commit builder has multiple copy actions for the same directory"
                                .into(),
                        ));
                    }
                    op.action = Some(DirAction::Copy {
                        from_path: copy.from_path,
                        from_rev,
                    });
                }
                NodeKind::None => {
                    return Err(SvnError::Protocol(format!(
                        "copy source does not exist at {}@{from_rev}",
                        copy.from_path
                    )));
                }
                NodeKind::Unknown => {
                    return Err(SvnError::Protocol(format!(
                        "copy source has unknown kind at {}@{from_rev}",
                        copy.from_path
                    )));
                }
            }
        }

        // Prevent edits inside a deleted subtree.
        for delete_path in &delete_paths {
            let prefix = format!("{delete_path}/");
            if file_ops.keys().any(|p| p.starts_with(&prefix))
                || dir_ops.keys().any(|p| p.starts_with(&prefix))
            {
                return Err(SvnError::Protocol(format!(
                    "cannot edit inside deleted path {delete_path}"
                )));
            }
            if file_ops.contains_key(delete_path) || dir_ops.contains_key(delete_path) {
                return Err(SvnError::Protocol(format!(
                    "delete conflicts with other changes at {delete_path}"
                )));
            }
        }

        let mut tasks = Vec::<Task>::new();
        for dir in dir_ops.keys() {
            tasks.push(Task::Dir(dir.clone()));
        }
        for file in file_ops.keys() {
            tasks.push(Task::File(file.clone()));
        }
        for delete_path in &delete_paths {
            tasks.push(Task::Delete(delete_path.clone()));
        }
        tasks.sort_by(|a, b| a.path().cmp(b.path()));

        // Determine which directories must be opened and whether they already exist.
        let mut required_dirs = BTreeSet::<String>::new();
        for task in &tasks {
            match task {
                Task::Dir(path) => {
                    for dir in dir_prefixes(path) {
                        required_dirs.insert(dir);
                    }
                }
                Task::File(path) | Task::Delete(path) => {
                    let parent = parent_dir(path);
                    for dir in dir_prefixes(&parent) {
                        required_dirs.insert(dir);
                    }
                }
            }
        }

        let mut dir_plans = BTreeMap::<String, DirPlanKind>::new();
        for dir in required_dirs {
            let dir = validate_rel_dir_path(&dir)?;
            let kind = session.check_path(&dir, Some(base_rev)).await?;
            match kind {
                NodeKind::Dir => {
                    if matches!(
                        dir_ops.get(&dir).and_then(|o| o.action.as_ref()),
                        Some(DirAction::Copy { .. })
                    ) {
                        return Err(SvnError::Protocol(format!(
                            "copy destination directory already exists at {dir}"
                        )));
                    }
                    dir_plans.insert(dir, DirPlanKind::Open);
                }
                NodeKind::None => {
                    let copy_from = match dir_ops.get(&dir).and_then(|o| o.action.as_ref()) {
                        Some(DirAction::Copy {
                            from_path,
                            from_rev,
                        }) => Some((from_path.clone(), *from_rev)),
                        _ => None,
                    };
                    dir_plans.insert(dir, DirPlanKind::Add { copy_from });
                }
                NodeKind::File | NodeKind::Unknown => {
                    return Err(SvnError::Protocol(format!(
                        "expected directory or none at {dir} (got {kind})"
                    )));
                }
            }
        }

        // Resolve file states (existence, copy destination constraints, prop-only constraints).
        let mut resolved_files = BTreeMap::<String, ResolvedFile>::new();
        for (path, op) in file_ops {
            let kind = session.check_path(&path, Some(base_rev)).await?;
            match kind {
                NodeKind::None | NodeKind::File => {}
                NodeKind::Dir | NodeKind::Unknown => {
                    return Err(SvnError::Protocol(format!(
                        "expected file or none at {path} (got {kind})"
                    )));
                }
            }
            let exists = kind == NodeKind::File;
            match op.action.as_ref() {
                None if !exists => {
                    return Err(SvnError::Protocol(format!(
                        "cannot set file properties on a missing file at {path}"
                    )));
                }
                Some(FileAction::Copy { .. }) if exists => {
                    return Err(SvnError::Protocol(format!(
                        "copy destination file already exists at {path}"
                    )));
                }
                _ => {}
            }

            resolved_files.insert(
                path.clone(),
                ResolvedFile {
                    path,
                    exists,
                    action: op.action,
                    props: op.props,
                },
            );
        }

        // Validate delete paths exist at base_rev.
        for delete_path in &delete_paths {
            let kind = session.check_path(delete_path, Some(base_rev)).await?;
            if matches!(kind, NodeKind::None | NodeKind::Unknown) {
                return Err(SvnError::Protocol(format!(
                    "delete target does not exist at {delete_path}"
                )));
            }
        }

        let mut token_gen = TokenGen::default();
        let root_token = "r".to_string();
        let mut stack: Vec<(String, String)> = vec![(String::new(), root_token.clone())];
        let mut commands = Vec::new();

        commands.push(EditorCommand::OpenRoot {
            rev: Some(base_rev),
            token: root_token.clone(),
        });

        for task in tasks {
            let target_dirs = match &task {
                Task::Dir(path) => dir_prefixes(path),
                Task::File(path) | Task::Delete(path) => {
                    let parent = parent_dir(path);
                    dir_prefixes(&parent)
                }
            };

            let mut lcp = 0usize;
            while lcp < target_dirs.len()
                && lcp + 1 < stack.len()
                && stack[lcp + 1].0 == target_dirs[lcp]
            {
                lcp += 1;
            }

            while stack.len() > lcp + 1 {
                let (_, token) = stack
                    .pop()
                    .ok_or_else(|| SvnError::Protocol("commit dir stack underflow".into()))?;
                commands.push(EditorCommand::CloseDir { dir_token: token });
            }

            for dir_path in &target_dirs[lcp..] {
                let parent_token = stack
                    .last()
                    .map(|(_, token)| token.clone())
                    .ok_or_else(|| SvnError::Protocol("missing parent dir token".into()))?;
                let token = token_gen.dir();
                let plan = dir_plans.get(dir_path).ok_or_else(|| {
                    SvnError::Protocol(format!("missing directory plan for '{dir_path}'"))
                })?;
                match plan {
                    DirPlanKind::Open => {
                        commands.push(EditorCommand::OpenDir {
                            path: dir_path.clone(),
                            parent_token,
                            child_token: token.clone(),
                            rev: base_rev,
                        });
                    }
                    DirPlanKind::Add { copy_from } => {
                        commands.push(EditorCommand::AddDir {
                            path: dir_path.clone(),
                            parent_token,
                            child_token: token.clone(),
                            copy_from: copy_from.clone(),
                        });
                    }
                }
                stack.push((dir_path.clone(), token));
            }

            match task {
                Task::Dir(path) => {
                    let Some(op) = dir_ops.get(&path) else {
                        continue;
                    };
                    let dir_token = if path.is_empty() {
                        root_token.clone()
                    } else {
                        stack
                            .last()
                            .map(|(_, token)| token.clone())
                            .ok_or_else(|| SvnError::Protocol("missing current dir token".into()))?
                    };
                    for prop in &op.props {
                        commands.push(EditorCommand::ChangeDirProp {
                            dir_token: dir_token.clone(),
                            name: prop.name.clone(),
                            value: prop.value.clone(),
                        });
                    }
                }
                Task::File(path) => {
                    let file = resolved_files
                        .get(&path)
                        .ok_or_else(|| SvnError::Protocol("missing file plan".into()))?;

                    let dir_token = stack
                        .last()
                        .map(|(_, token)| token.clone())
                        .ok_or_else(|| SvnError::Protocol("missing current dir token".into()))?;
                    let file_token = token_gen.file();

                    match file.action.as_ref() {
                        Some(FileAction::Copy {
                            from_path,
                            from_rev,
                        }) => {
                            commands.push(EditorCommand::AddFile {
                                path: file.path.clone(),
                                dir_token,
                                file_token: file_token.clone(),
                                copy_from: Some((from_path.clone(), *from_rev)),
                            });
                        }
                        Some(FileAction::Put(_)) | None => {
                            if file.exists {
                                commands.push(EditorCommand::OpenFile {
                                    path: file.path.clone(),
                                    dir_token,
                                    file_token: file_token.clone(),
                                    rev: base_rev,
                                });
                            } else {
                                commands.push(EditorCommand::AddFile {
                                    path: file.path.clone(),
                                    dir_token,
                                    file_token: file_token.clone(),
                                    copy_from: None,
                                });
                            }
                        }
                    }

                    if let Some(FileAction::Put(contents)) = file.action.as_ref() {
                        commands.push(EditorCommand::ApplyTextDelta {
                            file_token: file_token.clone(),
                            base_checksum: None,
                        });

                        let svndiff = encode_fulltext_with_options(
                            svndiff_version,
                            contents,
                            self.zlib_level,
                            self.window_size,
                        )?;
                        for chunk in svndiff.chunks(64 * 1024) {
                            commands.push(EditorCommand::TextDeltaChunk {
                                file_token: file_token.clone(),
                                chunk: chunk.to_vec(),
                            });
                        }
                        commands.push(EditorCommand::TextDeltaEnd {
                            file_token: file_token.clone(),
                        });
                    }

                    for prop in &file.props {
                        commands.push(EditorCommand::ChangeFileProp {
                            file_token: file_token.clone(),
                            name: prop.name.clone(),
                            value: prop.value.clone(),
                        });
                    }

                    commands.push(EditorCommand::CloseFile {
                        file_token,
                        text_checksum: None,
                    });
                }
                Task::Delete(path) => {
                    let parent_token = stack
                        .last()
                        .map(|(_, token)| token.clone())
                        .ok_or_else(|| SvnError::Protocol("missing current dir token".into()))?;
                    commands.push(EditorCommand::DeleteEntry {
                        path,
                        rev: base_rev,
                        dir_token: parent_token,
                    });
                }
            }
        }

        while stack.len() > 1 {
            let (_, token) = stack
                .pop()
                .ok_or_else(|| SvnError::Protocol("commit dir stack underflow".into()))?;
            commands.push(EditorCommand::CloseDir { dir_token: token });
        }

        commands.push(EditorCommand::CloseDir {
            dir_token: root_token,
        });
        commands.push(EditorCommand::CloseEdit);

        Ok(commands)
    }
}

impl Default for CommitBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl RaSvnSession {
    /// Runs `commit`, building a low-level editor drive from `builder`.
    ///
    /// ```rust,no_run
    /// # use svn::{CommitBuilder, CommitOptions, RaSvnSession};
    /// # async fn demo(session: &mut RaSvnSession, head: u64) -> svn::Result<()> {
    /// let builder = CommitBuilder::new()
    ///     .with_base_rev(head)
    ///     .put_file("trunk/hello.txt", b"hello from svn-rs\n".to_vec());
    /// session
    ///     .commit_with_builder(&CommitOptions::new("edit file contents"), &builder)
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn commit_with_builder(
        &mut self,
        options: &CommitOptions,
        builder: &CommitBuilder,
    ) -> Result<CommitInfo, SvnError> {
        let commands = builder.build_editor_commands(self).await?;
        self.commit(options, &commands).await
    }

    /// Runs `commit`, streaming file contents from a [`CommitStreamBuilder`].
    pub async fn commit_with_stream_builder(
        &mut self,
        options: &CommitOptions,
        builder: CommitStreamBuilder,
    ) -> Result<CommitInfo, SvnError> {
        builder.commit(self, options).await
    }
}

#[derive(Clone, Debug, Default)]
struct FileOp {
    action: Option<FileAction>,
    props: Vec<PropChange>,
}

#[derive(Clone, Debug)]
enum FileAction {
    Put(Vec<u8>),
    Copy { from_path: String, from_rev: u64 },
}

#[derive(Clone, Debug, Default)]
struct DirOp {
    action: Option<DirAction>,
    props: Vec<PropChange>,
}

#[derive(Clone, Debug)]
enum DirAction {
    Mkdir,
    Copy { from_path: String, from_rev: u64 },
}

#[derive(Clone, Debug)]
struct PropChange {
    name: String,
    value: Option<Vec<u8>>,
}

#[derive(Clone, Debug)]
struct CopyOp {
    from_path: String,
    from_rev: Option<u64>,
    to_path: String,
}

#[derive(Clone, Debug)]
struct ResolvedFile {
    path: String,
    exists: bool,
    action: Option<FileAction>,
    props: Vec<PropChange>,
}

#[derive(Clone, Debug)]
enum Task {
    Dir(String),
    File(String),
    Delete(String),
}

impl Task {
    fn path(&self) -> &str {
        match self {
            Task::Dir(p) | Task::File(p) | Task::Delete(p) => p.as_str(),
        }
    }
}

#[derive(Clone, Debug)]
enum DirPlanKind {
    Open,
    Add { copy_from: Option<(String, u64)> },
}

#[derive(Default)]
struct TokenGen {
    next_dir: u64,
    next_file: u64,
}

impl TokenGen {
    fn dir(&mut self) -> String {
        self.next_dir += 1;
        format!("d{}", self.next_dir)
    }

    fn file(&mut self) -> String {
        self.next_file += 1;
        format!("f{}", self.next_file)
    }
}

fn parent_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

fn dir_prefixes(dir: &str) -> Vec<String> {
    if dir.trim().is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut current = String::new();
    for part in dir.split('/') {
        if part.is_empty() {
            continue;
        }
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(part);
        out.push(current.clone());
    }
    out
}

fn select_svndiff_version(session: &RaSvnSession) -> SvndiffVersion {
    if session.has_capability(Capability::AcceptsSvndiff2) {
        SvndiffVersion::V2
    } else if session.has_capability(Capability::Svndiff1) {
        SvndiffVersion::V1
    } else {
        SvndiffVersion::V0
    }
}
