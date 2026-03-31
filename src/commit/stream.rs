use std::collections::{BTreeMap, BTreeSet};

use tokio::io::{AsyncRead, AsyncReadExt};

use crate::path::{validate_rel_dir_path, validate_rel_path};
use crate::svndiff::{SvndiffVersion, encode_insertion_window};
use crate::{
    Capability, CommitInfo, CommitOptions, EditorCommand, NodeKind, RaSvnSession, SvnError,
};

use super::util::{SvndiffMode, TokenGen, dir_prefixes, parent_dir, select_svndiff_version};

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

#[derive(Clone, Debug)]
enum DirPlanKind {
    Open,
    Add { copy_from: Option<(String, u64)> },
}
