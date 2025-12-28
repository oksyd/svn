use thiserror::Error;

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
/// One error entry as returned by a server-side `failure` response.
pub struct ServerErrorItem {
    /// Subversion error code.
    pub code: u64,
    /// Human-readable error message (UTF-8, lossy-decoded).
    pub message: Option<String>,
    /// Source file on the server side, if provided.
    pub file: Option<String>,
    /// Source line on the server side, if provided.
    pub line: Option<u64>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[non_exhaustive]
#[derive(Clone, Debug, PartialEq, Eq)]
/// A structured server error returned by `svnserve`.
///
/// `context` is typically the command name (or a higher-level operation) and
/// `chain` is the server-provided error stack.
pub struct ServerError {
    /// High-level context for the failure (for example, the command name).
    pub context: Option<String>,
    /// The server-provided error chain.
    pub chain: Vec<ServerErrorItem>,
}

impl ServerError {
    /// Attaches additional context to this error.
    pub fn with_context(mut self, context: impl Into<String>) -> Self {
        self.context = Some(context.into());
        self
    }

    /// Returns a single-line, human-readable message.
    ///
    /// This is a best-effort summary of the server-provided error chain.
    pub fn message_summary(&self) -> String {
        let mut messages = Vec::new();
        for err in &self.chain {
            if let Some(message) = err.message.as_deref()
                && !message.is_empty()
            {
                messages.push(message);
            }
        }
        if messages.is_empty() {
            "unknown error".to_string()
        } else {
            messages.join("; ")
        }
    }
}

impl std::fmt::Display for ServerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(ctx) = self.context.as_deref()
            && !ctx.is_empty()
        {
            write!(f, "{ctx}: ")?;
        }
        write!(f, "{}", self.message_summary())
    }
}

#[derive(Debug, Error)]
#[non_exhaustive]
/// Errors returned by this crate.
pub enum SvnError {
    /// The provided URL is syntactically invalid or unsupported.
    #[error("invalid svn url: {0}")]
    InvalidUrl(String),
    /// The provided repository path is invalid or unsafe.
    #[error("invalid path: {0}")]
    InvalidPath(String),
    /// An I/O error occurred while reading/writing the network stream.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// The server response did not match the expected `ra_svn` protocol shape.
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The server requested authentication but offered no supported mechanisms.
    #[error("auth required but no supported mechanism")]
    AuthUnavailable,
    /// Authentication failed (for example, invalid username/password).
    #[error("auth failed: {0}")]
    AuthFailed(String),
    /// The server returned a `failure` response.
    #[error("server error: {0}")]
    Server(ServerError),
}
