use super::*;

/// A reusable configuration object for connecting to an `svn://` server.
///
/// Use [`RaSvnClient::open_session`] to create a connected [`RaSvnSession`].
#[derive(Clone, Debug)]
pub struct RaSvnClient {
    pub(super) base_url: SvnUrl,
    pub(super) username: Option<String>,
    pub(super) password: Option<String>,
    pub(super) connect_timeout: Duration,
    pub(super) read_timeout: Duration,
    pub(super) write_timeout: Duration,
    pub(super) reconnect_retries: usize,
    pub(super) ra_client: String,
    #[cfg(feature = "ssh")]
    pub(super) ssh: Option<crate::ssh::SshConfig>,
}

/// A connected, stateful session to an `svn://` server.
///
/// A session owns a single TCP connection; operations require `&mut self` and
/// therefore run serially on that connection. Reuse a session if you want to
/// avoid reconnecting/handshaking for each operation.
pub struct RaSvnSession {
    pub(super) client: RaSvnClient,
    pub(super) conn: Option<RaSvnConnection>,
    pub(super) server_info: Option<ServerInfo>,
    pub(super) allow_reconnect: bool,
}

impl std::fmt::Debug for RaSvnSession {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RaSvnSession")
            .field("client", &self.client)
            .field("connected", &self.conn.is_some())
            .field("server_info", &self.server_info)
            .finish()
    }
}
