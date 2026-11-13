use super::*;

/// Connection settings and convenience operations for a Subversion repository.
///
/// Construction and cloning perform no network I/O. Call
/// [`open_session`](Self::open_session) to create a connected [`RaSvnSession`]
/// and reuse it for several operations. Convenience methods on this type open
/// a new session for each call.
///
/// Supports `svn://` and, with the `ssh` feature, `svn+ssh://`. For concurrency
/// with connection reuse, create a [`SessionPool`](crate::SessionPool).
/// See the [crate guide](crate) for a complete connection example.
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

/// A connected Subversion session anchored at a repository URL.
///
/// Created by [`RaSvnClient::open_session`] or
/// [`RaSvnClient::open_session_with_stream`]. A session owns one transport
/// connection; methods take `&mut self` and run sequentially. Use separate
/// sessions or a [`SessionPool`](crate::SessionPool) for concurrent requests.
///
/// File and directory paths are relative to [`client`](Self::client)'s base URL.
/// [`reparent`](Self::reparent) changes that base for subsequent requests.
/// See [paths and revisions](crate#paths-and-revisions) for examples.
///
/// # Errors and cancellation
///
/// Methods document their retry and partial-result behavior. Cancelling an
/// in-flight operation can leave unread protocol data on the connection.
/// Before reusing such a session, call [`reconnect`](Self::reconnect); for a
/// custom stream, create a replacement session instead. A pooled session is
/// returned on drop, so reconnect it before returning it to the pool after
/// cancellation. Dropping the pool guard alone does not discard the connection.
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
