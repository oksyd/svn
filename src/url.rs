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
    /// Full normalized URL string (`svn://host:port/path`, IPv6 uses brackets).
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
        if input.len() < "svn://".len() || !input[.."svn://".len()].eq_ignore_ascii_case("svn://") {
            return Err(SvnError::InvalidUrl(
                "only svn:// URLs are supported".to_string(),
            ));
        }

        let rest = &input["svn://".len()..];
        let (authority, path) = if let Some((authority, p)) = rest.split_once('/') {
            let path = &rest[(rest.len() - p.len() - 1)..];
            (authority, path)
        } else {
            (rest, "/")
        };

        let (host, port) = if let Some(authority) = authority.strip_prefix('[') {
            let Some(end) = authority.find(']') else {
                return Err(SvnError::InvalidUrl(format!("invalid url: {input}")));
            };
            let host = &authority[..end];
            if host.trim().is_empty() {
                return Err(SvnError::InvalidUrl(format!(
                    "missing host in url: {input}"
                )));
            }
            let after = &authority[end + 1..];
            if after.is_empty() {
                (host.to_string(), 3690)
            } else if let Some(port_str) = after.strip_prefix(':') {
                let port = port_str
                    .parse::<u16>()
                    .map_err(|_| SvnError::InvalidUrl(format!("invalid port in url: {input}")))?;
                (host.to_string(), port)
            } else {
                return Err(SvnError::InvalidUrl(format!("invalid url: {input}")));
            }
        } else {
            match authority.matches(':').count() {
                0 => (authority.to_string(), 3690),
                1 => {
                    let (h, port_str) = authority
                        .rsplit_once(':')
                        .ok_or_else(|| SvnError::InvalidUrl(format!("invalid url: {input}")))?;
                    let port = port_str.parse::<u16>().map_err(|_| {
                        SvnError::InvalidUrl(format!("invalid port in url: {input}"))
                    })?;
                    (h.to_string(), port)
                }
                _ => {
                    return Err(SvnError::InvalidUrl(
                        "IPv6 addresses must be enclosed in brackets (e.g. svn://[::1]/repo)"
                            .to_string(),
                    ));
                }
            }
        };

        if host.trim().is_empty() {
            return Err(SvnError::InvalidUrl(format!(
                "missing host in url: {input}"
            )));
        }

        let host_url = if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
            format!("[{host}]")
        } else {
            host.clone()
        };
        let url = format!("svn://{host_url}:{port}{path}");
        Ok(Self { host, port, url })
    }

    /// Returns a `host:port` string suitable for `TcpStream::connect`.
    ///
    /// IPv6 hosts are formatted with brackets.
    pub fn socket_addr(&self) -> String {
        let host = self.host.as_str();
        if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
            format!("[{host}]:{}", self.port)
        } else {
            format!("{host}:{}", self.port)
        }
    }
}

impl std::fmt::Display for SvnUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.url)
    }
}

impl std::str::FromStr for SvnUrl {
    type Err = SvnError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
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

    #[test]
    fn svn_url_parse_supports_ipv6_in_brackets() {
        let parsed = SvnUrl::parse("svn://[::1]/repo").unwrap();
        assert_eq!(parsed.host, "::1");
        assert_eq!(parsed.port, 3690);
        assert_eq!(parsed.url, "svn://[::1]:3690/repo");
        assert_eq!(parsed.socket_addr(), "[::1]:3690");

        let parsed = SvnUrl::parse("svn://[::1]:1234/repo").unwrap();
        assert_eq!(parsed.host, "::1");
        assert_eq!(parsed.port, 1234);
        assert_eq!(parsed.url, "svn://[::1]:1234/repo");
        assert_eq!(parsed.socket_addr(), "[::1]:1234");
    }

    #[test]
    fn svn_url_parse_rejects_unbracketed_ipv6() {
        let err = SvnUrl::parse("svn://::1/repo").unwrap_err();
        assert!(matches!(err, SvnError::InvalidUrl(_)));
    }

    #[test]
    fn svn_url_from_str_uses_parse_and_display_uses_normalized_url() {
        let url: SvnUrl = "svn://example.com/repo".parse().unwrap();
        assert_eq!(url.url, "svn://example.com:3690/repo");
        assert_eq!(url.to_string(), url.url);
        assert_eq!(url.socket_addr(), "example.com:3690");
    }
}
