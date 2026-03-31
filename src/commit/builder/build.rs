use super::plan::{
    CopyOp, DirAction, DirOp, DirPlanKind, FileAction, FileOp, PropChange, ResolvedFile, Task,
};
use super::*;

impl CommitBuilder {
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

        for delete_path in &delete_paths {
            let prefix = format!("{delete_path}/");
            if file_ops.keys().any(|path| path.starts_with(&prefix))
                || dir_ops.keys().any(|path| path.starts_with(&prefix))
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
        tasks.sort_by(|left, right| left.path().cmp(right.path()));

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
                        dir_ops.get(&dir).and_then(|op| op.action.as_ref()),
                        Some(DirAction::Copy { .. })
                    ) {
                        return Err(SvnError::Protocol(format!(
                            "copy destination directory already exists at {dir}"
                        )));
                    }
                    dir_plans.insert(dir, DirPlanKind::Open);
                }
                NodeKind::None => {
                    let copy_from = match dir_ops.get(&dir).and_then(|op| op.action.as_ref()) {
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
