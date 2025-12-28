use crate::SvnError;

pub(crate) fn validate_rel_path(path: &str) -> Result<String, SvnError> {
    let trimmed = path.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return Err(SvnError::InvalidPath("empty path".into()));
    }
    let path_ref = std::path::Path::new(trimmed);
    if path_ref.is_absolute()
        || path_ref
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(SvnError::InvalidPath("unsafe path".into()));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn validate_rel_dir_path(path: &str) -> Result<String, SvnError> {
    let trimmed = path.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        return Ok(String::new());
    }
    let path_ref = std::path::Path::new(trimmed);
    if path_ref.is_absolute()
        || path_ref
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(SvnError::InvalidPath("unsafe path".into()));
    }
    Ok(trimmed.to_string())
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
}
