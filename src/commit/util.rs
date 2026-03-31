use crate::svndiff::SvndiffVersion;
use crate::{Capability, RaSvnSession};

/// Svndiff version selection for [`super::CommitBuilder`].
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

#[derive(Default)]
pub(super) struct TokenGen {
    next_dir: u64,
    next_file: u64,
}

impl TokenGen {
    pub(super) fn dir(&mut self) -> String {
        self.next_dir += 1;
        format!("d{}", self.next_dir)
    }

    pub(super) fn file(&mut self) -> String {
        self.next_file += 1;
        format!("f{}", self.next_file)
    }
}

pub(super) fn parent_dir(path: &str) -> String {
    match path.rsplit_once('/') {
        Some((dir, _)) => dir.to_string(),
        None => String::new(),
    }
}

pub(super) fn dir_prefixes(dir: &str) -> Vec<String> {
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

pub(super) fn select_svndiff_version(session: &RaSvnSession) -> SvndiffVersion {
    if session.has_capability(Capability::AcceptsSvndiff2) {
        SvndiffVersion::V2
    } else if session.has_capability(Capability::Svndiff1) {
        SvndiffVersion::V1
    } else {
        SvndiffVersion::V0
    }
}
