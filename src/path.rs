use crate::SvnError;

use std::borrow::Cow;

fn canonicalize_rel_path(path: &str, allow_empty: bool) -> Result<Cow<'_, str>, SvnError> {
    let raw = path.trim();

    #[cfg(windows)]
    if raw.starts_with("\\\\") {
        return Err(SvnError::InvalidPath("unsafe path".into()));
    }

    let trimmed = raw.trim_matches(['/', '\\']);

    if trimmed.is_empty() {
        if allow_empty {
            return Ok(Cow::Borrowed(""));
        }
        return Err(SvnError::InvalidPath("empty path".into()));
    }

    #[cfg(windows)]
    if let Some((first, rest)) = trimmed.as_bytes().split_first()
        && rest.first() == Some(&b':')
        && first.is_ascii_alphabetic()
    {
        return Err(SvnError::InvalidPath("unsafe path".into()));
    }

    if trimmed.contains('\0') {
        return Err(SvnError::InvalidPath("unsafe path".into()));
    }

    let mut parts: Vec<&str> = Vec::new();
    let mut needs_alloc = trimmed.contains('\\');

    for seg in trimmed.split(['/', '\\']) {
        if seg.is_empty() {
            needs_alloc = true;
            continue;
        }
        if seg == "." {
            needs_alloc = true;
            continue;
        }
        if seg == ".." {
            return Err(SvnError::InvalidPath("unsafe path".into()));
        }
        parts.push(seg);
    }

    if parts.is_empty() {
        if allow_empty {
            return Ok(Cow::Borrowed(""));
        }
        return Err(SvnError::InvalidPath("empty path".into()));
    }

    if !needs_alloc {
        return Ok(Cow::Borrowed(trimmed));
    }

    Ok(Cow::Owned(parts.join("/")))
}

pub(crate) fn validate_rel_path(path: &str) -> Result<String, SvnError> {
    Ok(canonicalize_rel_path(path, false)?.into_owned())
}

pub(crate) fn validate_rel_dir_path(path: &str) -> Result<String, SvnError> {
    Ok(canonicalize_rel_path(path, true)?.into_owned())
}

pub(crate) fn validate_rel_path_ref(path: &str) -> Result<Cow<'_, str>, SvnError> {
    canonicalize_rel_path(path, false)
}

pub(crate) fn validate_rel_dir_path_ref(path: &str) -> Result<Cow<'_, str>, SvnError> {
    canonicalize_rel_path(path, true)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn validate_rel_path_rejects_empty_path() {
        let err = validate_rel_path("  / ").unwrap_err();
        assert!(matches!(err, SvnError::InvalidPath(_)));
    }

    #[test]
    fn validate_rel_path_rejects_parent_dir() {
        assert!(validate_rel_path("../a.zip").is_err());
        assert!(validate_rel_path("a/../b.zip").is_err());
    }

    #[test]
    fn validate_rel_path_normalizes_leading_slash() {
        assert_eq!(validate_rel_path("trunk/a.zip").unwrap(), "trunk/a.zip");
        assert_eq!(validate_rel_path("/trunk/a.zip").unwrap(), "trunk/a.zip");
    }

    #[test]
    fn validate_rel_path_drops_trailing_slash() {
        assert_eq!(validate_rel_path("trunk/").unwrap(), "trunk");
        assert_eq!(validate_rel_path("/trunk/").unwrap(), "trunk");
    }

    #[test]
    fn validate_rel_path_collapses_redundant_separators_and_curdir() {
        assert_eq!(
            validate_rel_path("//trunk//./a.zip").unwrap(),
            "trunk/a.zip"
        );
        assert_eq!(
            validate_rel_path("trunk\\\\sub\\\\.\\\\a.zip").unwrap(),
            "trunk/sub/a.zip"
        );
    }

    #[test]
    fn validate_rel_dir_path_allows_empty_root() {
        assert_eq!(validate_rel_dir_path("").unwrap(), "");
        assert_eq!(validate_rel_dir_path("/").unwrap(), "");
    }

    #[test]
    fn validate_rel_dir_path_rejects_parent_dir() {
        assert!(validate_rel_dir_path("../").is_err());
        assert!(validate_rel_dir_path("a/../b").is_err());
    }

    #[test]
    fn validate_rel_dir_path_normalizes_leading_slash() {
        assert_eq!(validate_rel_dir_path("trunk").unwrap(), "trunk");
        assert_eq!(validate_rel_dir_path("/trunk").unwrap(), "trunk");
        assert_eq!(validate_rel_dir_path("/trunk/dir").unwrap(), "trunk/dir");
    }

    #[cfg(windows)]
    #[test]
    fn validate_rel_path_rejects_unc_and_drive_prefix() {
        assert!(validate_rel_path(r"\\server\share\trunk\file.txt").is_err());
        assert!(validate_rel_path(r"C:\trunk\file.txt").is_err());
        assert!(validate_rel_path(r"C:trunk\file.txt").is_err());
    }
}
