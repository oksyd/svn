use super::*;

pub(super) fn verify_empty_checksum(expected: &str) -> Result<(), SvnError> {
    let expected = expected.strip_prefix("md5:").unwrap_or(expected);
    if expected.eq_ignore_ascii_case("d41d8cd98f00b204e9800998ecf8427e") {
        Ok(())
    } else {
        Err(SvnError::Protocol("empty base checksum mismatch".into()))
    }
}

pub(super) fn verify_checksum(path: &Path, expected: &str) -> Result<(), SvnError> {
    let expected = expected.strip_prefix("md5:").unwrap_or(expected);
    if expected.len() != 32 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(SvnError::Protocol("unsupported file checksum".into()));
    }
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Md5::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    if !hex::encode(hasher.finalize()).eq_ignore_ascii_case(expected) {
        return Err(SvnError::Protocol("file checksum mismatch".into()));
    }
    Ok(())
}

pub(super) async fn verify_checksum_async(path: &Path, expected: &str) -> Result<(), SvnError> {
    use tokio::io::AsyncReadExt;

    let expected = expected.strip_prefix("md5:").unwrap_or(expected);
    if expected.len() != 32 || !expected.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(SvnError::Protocol("unsupported file checksum".into()));
    }
    let mut file = tokio::fs::File::open(path).await?;
    let mut hasher = Md5::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    if !hex::encode(hasher.finalize()).eq_ignore_ascii_case(expected) {
        return Err(SvnError::Protocol("file checksum mismatch".into()));
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct ExportState {
    root: PathBuf,
    strip_prefix: Option<String>,
    dir_tokens: HashMap<String, PathBuf>,
    file_tokens: HashMap<String, PathBuf>,
    file_copy_from: HashMap<String, PathBuf>,
    file_added: HashSet<String>,
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
            file_added: HashSet::new(),
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

    pub(super) fn open_root(&mut self, token: String) -> Result<(), SvnError> {
        self.open_dir(token, self.root.clone())
    }

    pub(super) fn open_dir(&mut self, token: String, dir: PathBuf) -> Result<(), SvnError> {
        if self.dir_tokens.contains_key(&token) {
            return Err(SvnError::Protocol(format!(
                "directory token '{token}' reused before close-dir"
            )));
        }
        self.dir_tokens.insert(token, dir);
        Ok(())
    }

    pub(super) fn close_dir(&mut self, token: &str) -> Result<(), SvnError> {
        self.dir_tokens
            .remove(token)
            .map(|_| ())
            .ok_or_else(|| SvnError::Protocol(format!("close-dir for unknown token '{token}'")))
    }

    pub(super) fn ensure_dir_token(&self, token: &str) -> Result<(), SvnError> {
        if self.dir_tokens.contains_key(token) {
            Ok(())
        } else {
            Err(SvnError::Protocol(format!(
                "editor event references unknown directory token '{token}'"
            )))
        }
    }

    pub(super) fn track_file(
        &mut self,
        token: String,
        dest: PathBuf,
        copy_from: Option<PathBuf>,
        added: bool,
    ) -> Result<(), SvnError> {
        if self.file_tokens.contains_key(&token) {
            return Err(SvnError::Protocol(format!(
                "file token '{token}' reused before close-file"
            )));
        }
        match copy_from {
            Some(src) => {
                self.file_copy_from.insert(token.clone(), src);
            }
            None => {
                let _ = self.file_copy_from.remove(&token);
            }
        }
        if added {
            self.file_added.insert(token.clone());
        } else {
            let _ = self.file_added.remove(&token);
        }
        self.file_tokens.insert(token, dest);
        Ok(())
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

    pub(super) fn ensure_file_token(&self, token: &str) -> Result<(), SvnError> {
        if self.file_tokens.contains_key(token) {
            Ok(())
        } else {
            Err(SvnError::Protocol(format!(
                "editor event references unknown file token '{token}'"
            )))
        }
    }

    pub(super) fn file_copy_source(&self, token: &str) -> Option<&PathBuf> {
        self.file_copy_from.get(token)
    }

    pub(super) fn file_was_added(&self, token: &str) -> bool {
        self.file_added.contains(token)
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
        let _ = self.file_added.remove(token);
        let _ = self.file_tokens.remove(token);
        #[cfg(unix)]
        let _ = self.exec_tokens.remove(token);
    }

    pub(super) fn reset(&mut self) {
        self.dir_tokens.clear();
        self.file_tokens.clear();
        self.file_copy_from.clear();
        self.file_added.clear();
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExistingPathKind {
    File,
    Dir,
}

fn existing_path_kind(path: &Path) -> Result<Option<ExistingPathKind>, SvnError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            if is_symlink_like(&meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to write through a symlink/reparse point".into(),
                ));
            }
            if meta.is_file() {
                Ok(Some(ExistingPathKind::File))
            } else if meta.is_dir() {
                Ok(Some(ExistingPathKind::Dir))
            } else {
                Err(SvnError::InvalidPath(
                    "refusing to operate on an unknown file type".into(),
                ))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

async fn existing_path_kind_async(path: &Path) -> Result<Option<ExistingPathKind>, SvnError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(meta) => {
            if is_symlink_like(&meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to write through a symlink/reparse point".into(),
                ));
            }
            if meta.is_file() {
                Ok(Some(ExistingPathKind::File))
            } else if meta.is_dir() {
                Ok(Some(ExistingPathKind::Dir))
            } else {
                Err(SvnError::InvalidPath(
                    "refusing to operate on an unknown file type".into(),
                ))
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
    }
}

fn ensure_root_not_symlink(root: &Path) -> Result<(), SvnError> {
    match std::fs::symlink_metadata(root) {
        Ok(meta) => {
            if is_symlink_like(&meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to use a symlink/reparse point as export root".into(),
                ));
            }
            if !meta.is_dir() {
                return Err(SvnError::InvalidPath(
                    "export root exists but is not a directory".into(),
                ));
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

async fn ensure_root_not_symlink_async(root: &Path) -> Result<(), SvnError> {
    match tokio::fs::symlink_metadata(root).await {
        Ok(meta) => {
            if is_symlink_like(&meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to use a symlink/reparse point as export root".into(),
                ));
            }
            if !meta.is_dir() {
                return Err(SvnError::InvalidPath(
                    "export root exists but is not a directory".into(),
                ));
            }
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err.into()),
    }
}

pub(super) fn ensure_no_symlink_prefix(root: &Path, path: &Path) -> Result<(), SvnError> {
    ensure_root_not_symlink(root)?;

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
    ensure_root_not_symlink_async(root).await?;

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
    ensure_root_not_symlink(root)?;

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
    ensure_root_not_symlink_async(root).await?;

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

pub(super) fn file_exists_no_symlink(root: &Path, path: &Path) -> Result<bool, SvnError> {
    ensure_no_symlink_prefix(root, path)?;

    match existing_path_kind(path)? {
        Some(ExistingPathKind::File) => Ok(true),
        Some(ExistingPathKind::Dir) => Err(SvnError::InvalidPath(
            "refusing to treat a non-file as a file".into(),
        )),
        None => Ok(false),
    }
}

pub(super) async fn file_exists_no_symlink_async(
    root: &Path,
    path: &Path,
) -> Result<bool, SvnError> {
    ensure_no_symlink_prefix_async(root, path).await?;

    match existing_path_kind_async(path).await? {
        Some(ExistingPathKind::File) => Ok(true),
        Some(ExistingPathKind::Dir) => Err(SvnError::InvalidPath(
            "refusing to treat a non-file as a file".into(),
        )),
        None => Ok(false),
    }
}

pub(super) fn copy_file_no_symlink(root: &Path, src: &Path, dest: &Path) -> Result<bool, SvnError> {
    ensure_no_symlink_prefix(root, src)?;

    match std::fs::symlink_metadata(src) {
        Ok(meta) => {
            if is_symlink_like(&meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a symlink/reparse point".into(),
                ));
            }
            if !meta.is_file() {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a non-file as a file".into(),
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    }

    if let Some(parent) = dest.parent() {
        create_dir_all_no_symlink(root, parent)?;
    }
    file_exists_no_symlink(root, dest)?;
    let _ = std::fs::copy(src, dest)?;
    Ok(true)
}

pub(super) async fn copy_file_no_symlink_async(
    root: &Path,
    src: &Path,
    dest: &Path,
) -> Result<bool, SvnError> {
    ensure_no_symlink_prefix_async(root, src).await?;

    match tokio::fs::symlink_metadata(src).await {
        Ok(meta) => {
            if is_symlink_like(&meta) {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a symlink/reparse point".into(),
                ));
            }
            if !meta.is_file() {
                return Err(SvnError::InvalidPath(
                    "refusing to copy a non-file as a file".into(),
                ));
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    }

    if let Some(parent) = dest.parent() {
        create_dir_all_no_symlink_async(root, parent).await?;
    }
    file_exists_no_symlink_async(root, dest).await?;
    let _ = tokio::fs::copy(src, dest).await?;
    Ok(true)
}

pub(super) fn create_empty_file_no_symlink(root: &Path, dest: &Path) -> Result<(), SvnError> {
    if let Some(parent) = dest.parent() {
        create_dir_all_no_symlink(root, parent)?;
    }
    file_exists_no_symlink(root, dest)?;
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dest)?;
    Ok(())
}

pub(super) async fn create_empty_file_no_symlink_async(
    root: &Path,
    dest: &Path,
) -> Result<(), SvnError> {
    if let Some(parent) = dest.parent() {
        create_dir_all_no_symlink_async(root, parent).await?;
    }
    file_exists_no_symlink_async(root, dest).await?;
    let _ = tokio::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(dest)
        .await?;
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
