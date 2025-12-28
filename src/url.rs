use crate::SvnError;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// A normalized `svn://` URL.
///
/// This crate supports `svn://` only. The parsed URL is normalized to include an
/// explicit port (defaulting to `3690`) and an explicit path (defaulting to
/// `/`).
pub struct SvnUrl {
    /// Hostname (or IP) portion of the URL.
    pub host: String,
    /// TCP port portion of the URL.
    pub port: u16,
    /// Full normalized URL string (`svn://host:port/path`).
    pub url: String,
}

impl SvnUrl {
    /// Parses and normalizes a `svn://` URL.
    ///
    /// # Examples
    ///
    /// ```
    /// # use svn::SvnUrl;
    /// let url = SvnUrl::parse("svn://example.com/repo").unwrap();
    /// assert_eq!(url.url, "svn://example.com:3690/repo");
    /// ```
    pub fn parse(input: &str) -> Result<Self, SvnError> {
        let input = input.trim();
        if !input.to_ascii_lowercase().starts_with("svn://") {
            return Err(SvnError::InvalidUrl(
                "only svn:// URLs are supported".to_string(),
            ));
        }

        let mut rest = &input["svn://".len()..];
        let mut path = "/";
        if let Some((authority, p)) = rest.split_once('/') {
            rest = authority;
            path = &input[(input.len() - p.len() - 1)..];
        }

        let (host, port) = if let Some((h, port_str)) = rest.rsplit_once(':') {
            let port = port_str
                .parse::<u16>()
                .map_err(|_| SvnError::InvalidUrl(format!("invalid port in url: {input}")))?;
            (h.to_string(), port)
        } else {
            (rest.to_string(), 3690)
        };

        if host.trim().is_empty() {
            return Err(SvnError::InvalidUrl(format!(
                "missing host in url: {input}"
            )));
        }

        let url = format!("svn://{host}:{port}{path}");
        Ok(Self { host, port, url })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn svn_url_parse_supports_svn_only() {
        let err = SvnUrl::parse("http://example.com/repo").unwrap_err();
        assert!(matches!(err, SvnError::InvalidUrl(_)));
    }

    #[test]
    fn svn_url_parse_defaults_port_and_preserves_path() {
        let parsed = SvnUrl::parse("svn://example.com/repo").unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.port, 3690);
        assert_eq!(parsed.url, "svn://example.com:3690/repo");

        let parsed = SvnUrl::parse("svn://example.com").unwrap();
        assert_eq!(parsed.url, "svn://example.com:3690/");
    }

    #[test]
    fn svn_url_parse_accepts_explicit_port() {
        let parsed = SvnUrl::parse("svn://example.com:1234/repo").unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.port, 1234);
        assert_eq!(parsed.url, "svn://example.com:1234/repo");
    }

    #[test]
    fn svn_url_parse_rejects_invalid_port() {
        let err = SvnUrl::parse("svn://example.com:abc/repo").unwrap_err();
        assert!(matches!(err, SvnError::InvalidUrl(_)));
        let err = SvnUrl::parse("svn://example.com:70000/repo").unwrap_err();
        assert!(matches!(err, SvnError::InvalidUrl(_)));
    }

    #[test]
    fn svn_url_parse_rejects_missing_host() {
        let err = SvnUrl::parse("svn:///repo").unwrap_err();
        assert!(matches!(err, SvnError::InvalidUrl(_)));
    }

    #[test]
    fn svn_url_parse_trims_and_accepts_uppercase_scheme() {
        let parsed = SvnUrl::parse("  SVN://example.com/repo  ").unwrap();
        assert_eq!(parsed.url, "svn://example.com:3690/repo");
    }
}
