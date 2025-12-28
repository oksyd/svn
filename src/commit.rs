use std::collections::BTreeSet;

use crate::path::{validate_rel_dir_path, validate_rel_path};
use crate::svndiff::{SvndiffVersion, encode_fulltext_with_options};
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
/// This builder generates a low-level editor command sequence (including
/// `apply-textdelta` + `textdelta-chunk`) so callers don't need to manually
/// craft svndiff chunks.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct CommitBuilder {
    base_rev: Option<u64>,
    svndiff: SvndiffMode,
    zlib_level: u32,
    window_size: usize,
    changes: Vec<Change>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
enum Change {
    PutFile { path: String, contents: Vec<u8> },
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

        let mut files = Vec::new();
        for change in &self.changes {
            match change {
                Change::PutFile { path, contents } => {
                    let path = validate_rel_path(path)?;
                    files.push((path, contents.clone()));
                }
            }
        }

        // Ensure all required directories exist, and determine which files need
        // to be added vs opened.
        let mut required_dirs = BTreeSet::<String>::new();
        for (path, _) in &files {
            let parent = parent_dir(path);
            for dir in dir_prefixes(&parent) {
                required_dirs.insert(dir);
            }
        }

        for dir in &required_dirs {
            let dir_norm = validate_rel_dir_path(dir)?;
            let kind = session.check_path(&dir_norm, Some(base_rev)).await?;
            if kind != NodeKind::Dir {
                return Err(SvnError::Protocol(format!(
                    "expected directory at {dir_norm} (got {kind})"
                )));
            }
        }

        let mut file_states = Vec::new();
        for (path, contents) in files {
            let kind = session.check_path(&path, Some(base_rev)).await?;
            match kind {
                NodeKind::None | NodeKind::File => {}
                NodeKind::Dir | NodeKind::Unknown => {
                    return Err(SvnError::Protocol(format!(
                        "expected file or none at {path} (got {kind})"
                    )));
                }
            }
            file_states.push(FileChange {
                path,
                exists: kind == NodeKind::File,
                contents,
            });
        }

        file_states.sort_by(|a, b| a.path.cmp(&b.path));

        let mut token_gen = TokenGen::default();
        let root_token = "r".to_string();
        let mut stack: Vec<(String, String)> = vec![(String::new(), root_token.clone())];
        let mut commands = Vec::new();

        commands.push(EditorCommand::OpenRoot {
            rev: Some(base_rev),
            token: root_token.clone(),
        });

        for file in file_states {
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
                commands.push(EditorCommand::OpenDir {
                    path: dir_path.clone(),
                    parent_token,
                    child_token: token.clone(),
                    rev: base_rev,
                });
                stack.push((dir_path.clone(), token));
            }

            let dir_token = stack
                .last()
                .map(|(_, token)| token.clone())
                .ok_or_else(|| SvnError::Protocol("missing current dir token".into()))?;
            let file_token = token_gen.file();

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

            commands.push(EditorCommand::ApplyTextDelta {
                file_token: file_token.clone(),
                base_checksum: None,
            });

            let svndiff = encode_fulltext_with_options(
                svndiff_version,
                &file.contents,
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

            commands.push(EditorCommand::CloseFile {
                file_token,
                text_checksum: None,
            });
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
}

#[derive(Clone, Debug)]
struct FileChange {
    path: String,
    exists: bool,
    contents: Vec<u8>,
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
