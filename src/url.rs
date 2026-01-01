use crate::SvnError;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// A normalized Subversion repository URL.
///
/// Supported schemes:
///
/// - `svn://` (default port `3690`)
/// - `svn+ssh://` (default port `22`)
///
/// The parsed URL is normalized to include an explicit port and an explicit
/// path (defaulting to `/`).
pub struct SvnUrl {
    /// Hostname (or IP) portion of the URL.
    pub host: String,
    /// TCP port portion of the URL.
    pub port: u16,
    /// Full normalized URL string (`scheme://[user@]host:port/path`, IPv6 uses brackets).
    pub url: String,
}

impl SvnUrl {
    /// Parses and normalizes a `svn://` or `svn+ssh://` URL.
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
        let (scheme, rest, default_port) = if input.len() >= "svn+ssh://".len()
            && input[.."svn+ssh://".len()].eq_ignore_ascii_case("svn+ssh://")
        {
            ("svn+ssh://", &input["svn+ssh://".len()..], 22u16)
        } else if input.len() >= "svn://".len()
            && input[.."svn://".len()].eq_ignore_ascii_case("svn://")
        {
            ("svn://", &input["svn://".len()..], 3690u16)
        } else {
            return Err(SvnError::InvalidUrl(
                "only svn:// and svn+ssh:// URLs are supported".to_string(),
            ));
        };
        let (authority, path) = if let Some((authority, p)) = rest.split_once('/') {
            let path = &rest[(rest.len() - p.len() - 1)..];
            (authority, path)
        } else {
            (rest, "/")
        };

        let (username, hostport) = if let Some((user, hostport)) = authority.rsplit_once('@') {
            if user.contains(':') {
                return Err(SvnError::InvalidUrl(
                    "URL passwords are not supported (use user@host, not user:pass@host)"
                        .to_string(),
                ));
            }
            if user.trim().is_empty() {
                return Err(SvnError::InvalidUrl(format!(
                    "invalid url (empty username): {input}"
                )));
            }
            (Some(user.to_string()), hostport)
        } else {
            (None, authority)
        };

        let (host, port) = if let Some(hostport) = hostport.strip_prefix('[') {
            let Some(end) = hostport.find(']') else {
                return Err(SvnError::InvalidUrl(format!("invalid url: {input}")));
            };
            let host = &hostport[..end];
            if host.trim().is_empty() {
                return Err(SvnError::InvalidUrl(format!(
                    "missing host in url: {input}"
                )));
            }
            let after = &hostport[end + 1..];
            if after.is_empty() {
                (host.to_string(), default_port)
            } else if let Some(port_str) = after.strip_prefix(':') {
                let port = port_str
                    .parse::<u16>()
                    .map_err(|_| SvnError::InvalidUrl(format!("invalid port in url: {input}")))?;
                (host.to_string(), port)
            } else {
                return Err(SvnError::InvalidUrl(format!("invalid url: {input}")));
            }
        } else {
            match hostport.matches(':').count() {
                0 => (hostport.to_string(), default_port),
                1 => {
                    let (h, port_str) = hostport
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
        let user_url = username
            .as_deref()
            .map(|u| format!("{u}@"))
            .unwrap_or_default();
        let url = format!("{scheme}{user_url}{host_url}:{port}{path}");
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
    fn svn_url_parse_rejects_unknown_schemes() {
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
    fn svn_url_parse_supports_svn_plus_ssh() {
        let parsed = SvnUrl::parse("svn+ssh://example.com/repo").unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.port, 22);
        assert_eq!(parsed.url, "svn+ssh://example.com:22/repo");
    }

    #[test]
    fn svn_url_parse_supports_username_in_authority() {
        let parsed = SvnUrl::parse("svn+ssh://alice@example.com/repo").unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.port, 22);
        assert_eq!(parsed.url, "svn+ssh://alice@example.com:22/repo");

        let parsed = SvnUrl::parse("svn://alice@example.com/repo").unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.port, 3690);
        assert_eq!(parsed.url, "svn://alice@example.com:3690/repo");
    }

    #[test]
    fn svn_url_parse_rejects_passwords_in_userinfo() {
        let err = SvnUrl::parse("svn+ssh://alice:secret@example.com/repo").unwrap_err();
        assert!(matches!(err, SvnError::InvalidUrl(_)));
    }

    #[test]
    fn svn_url_parse_accepts_explicit_port() {
        let parsed = SvnUrl::parse("svn://example.com:1234/repo").unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.port, 1234);
        assert_eq!(parsed.url, "svn://example.com:1234/repo");

        let parsed = SvnUrl::parse("svn+ssh://example.com:2222/repo").unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.port, 2222);
        assert_eq!(parsed.url, "svn+ssh://example.com:2222/repo");
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
