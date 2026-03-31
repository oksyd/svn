use super::*;

#[derive(Debug)]
pub(super) struct ExportState {
    root: PathBuf,
    strip_prefix: Option<String>,
    dir_tokens: HashMap<String, PathBuf>,
    file_tokens: HashMap<String, PathBuf>,
    file_copy_from: HashMap<String, PathBuf>,
    next_tmp_id: u64,
    #[cfg(unix)]
    exec_tokens: HashMap<String, bool>,
}

impl ExportState {
    pub(super) fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            strip_prefix: None,
            dir_tokens: HashMap::new(),
            file_tokens: HashMap::new(),
            file_copy_from: HashMap::new(),
            next_tmp_id: 0,
            #[cfg(unix)]
            exec_tokens: HashMap::new(),
        }
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn with_strip_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.strip_prefix = Some(prefix.into());
        self
    }

    pub(super) fn repo_path_to_fs(
        &self,
        path: &str,
        allow_empty: bool,
    ) -> Result<PathBuf, SvnError> {
        map_repo_path_to_fs(&self.root, self.strip_prefix.as_deref(), path, allow_empty)
    }

    pub(super) fn new_tmp_path(&mut self, dest: &Path, token: &str) -> PathBuf {
        new_tmp_path(&self.root, dest, token, &mut self.next_tmp_id)
    }

    pub(super) fn open_root(&mut self, token: String) {
        self.dir_tokens.insert(token, self.root.clone());
    }

    pub(super) fn open_dir(&mut self, token: String, dir: PathBuf) {
        self.dir_tokens.insert(token, dir);
    }

    pub(super) fn close_dir(&mut self, token: &str) {
        let _ = self.dir_tokens.remove(token);
    }

    pub(super) fn track_file(&mut self, token: String, dest: PathBuf, copy_from: Option<PathBuf>) {
        if let Some(src) = copy_from {
            self.file_copy_from.insert(token.clone(), src);
        }
        self.file_tokens.insert(token, dest);
    }

    pub(super) fn file_dest(&self, token: &str) -> Result<PathBuf, SvnError> {
        self.file_tokens
            .get(token)
            .cloned()
            .ok_or_else(|| SvnError::Protocol("apply-textdelta for unknown file token".into()))
    }

    pub(super) fn file_dest_if_known(&self, token: &str) -> Option<PathBuf> {
        self.file_tokens.get(token).cloned()
    }

    pub(super) fn file_copy_source(&self, token: &str) -> Option<&PathBuf> {
        self.file_copy_from.get(token)
    }

    #[cfg(unix)]
    pub(super) fn record_exec(&mut self, token: &str, enabled: bool) {
        self.exec_tokens.insert(token.to_string(), enabled);
    }

    #[cfg(unix)]
    pub(super) fn take_exec(&mut self, token: &str) -> Option<bool> {
        self.exec_tokens.remove(token)
    }

    pub(super) fn clear_file(&mut self, token: &str) {
        let _ = self.file_copy_from.remove(token);
        let _ = self.file_tokens.remove(token);
        #[cfg(unix)]
        let _ = self.exec_tokens.remove(token);
    }

    pub(super) fn reset(&mut self) {
        self.dir_tokens.clear();
        self.file_tokens.clear();
        self.file_copy_from.clear();
        #[cfg(unix)]
        self.exec_tokens.clear();
    }
}

pub(super) fn map_repo_path_to_fs(
    root: &Path,
    strip_prefix: Option<&str>,
    path: &str,
    allow_empty: bool,
) -> Result<PathBuf, SvnError> {
    let canonical_path = validate_rel_dir_path_ref(path)?;
    let mut trimmed = canonical_path.as_ref();

    if let Some(prefix) = strip_prefix {
        let canonical_prefix = validate_rel_dir_path_ref(prefix)?;
        let prefix = canonical_prefix.as_ref();
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

    if trimmed.contains(':') {
        return Err(SvnError::InvalidPath("unsafe path".into()));
    }

    let mut out = root.to_path_buf();
    for part in trimmed.split('/') {
        out.push(part);
    }
    Ok(out)
}

pub(super) fn is_symlink_like(meta: &std::fs::Metadata) -> bool {
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

pub(super) fn ensure_no_symlink_prefix(root: &Path, path: &Path) -> Result<(), SvnError> {
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

pub(super) async fn ensure_no_symlink_prefix_async(
    root: &Path,
    path: &Path,
) -> Result<(), SvnError> {
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

pub(super) fn create_dir_all_no_symlink(root: &Path, dir: &Path) -> Result<(), SvnError> {
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

pub(super) async fn create_dir_all_no_symlink_async(
    root: &Path,
    dir: &Path,
) -> Result<(), SvnError> {
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

pub(super) fn new_tmp_path(
    root: &Path,
    dest: &Path,
    token: &str,
    next_tmp_id: &mut u64,
) -> PathBuf {
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

pub(super) fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), SvnError> {
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

pub(super) async fn copy_dir_recursive_async(src: &Path, dest: &Path) -> Result<(), SvnError> {
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

pub(super) fn copy_dir_missing_recursive(src: &Path, dest: &Path) -> Result<(), SvnError> {
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

pub(super) async fn copy_dir_missing_recursive_async(
    src: &Path,
    dest: &Path,
) -> Result<(), SvnError> {
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

#[cfg(unix)]
pub(super) fn apply_executable_bit(path: &Path, exec: bool) -> Result<(), SvnError> {
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
pub(super) async fn apply_executable_bit_async(path: &Path, exec: bool) -> Result<(), SvnError> {
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
