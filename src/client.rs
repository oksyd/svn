use std::fmt::Formatter;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tracing::debug;

use crate::path::{validate_rel_dir_path, validate_rel_path};
use crate::rasvn::conn::{RaSvnConnection, RaSvnConnectionConfig};
use crate::rasvn::edit::{drive_editor, encode_editor_command, parse_failure, send_report};
use crate::rasvn::parse::{
    opt_tuple_wordish, parse_commit_info, parse_file_rev_entry, parse_get_dir_listing,
    parse_get_file_response_params, parse_iproplist, parse_list_dirent, parse_location_entry,
    parse_location_segment, parse_lockdesc, parse_log_entry, parse_mergeinfo_catalog,
    parse_proplist, parse_stat_params,
};
use crate::raw::SvnItem;
use crate::{
    Capability, CommitInfo, CommitOptions, Depth, DiffOptions, DirEntry, DirListing, DirentField,
    EditorCommand, EditorEvent, EditorEventHandler, GetFileOptions, GetFileResult, InheritedProps,
    ListOptions, LocationEntry, LocationSegment, LockDesc, LockManyOptions, LockOptions,
    LockTarget, LogEntry, LogOptions, LogRevProps, MergeInfoCatalog, MergeInfoInheritance,
    NodeKind, PropertyList, ReplayOptions, ReplayRangeOptions, Report, ReportCommand, ServerInfo,
    StatEntry, StatusOptions, SvnError, SvnUrl, SwitchOptions, UnlockManyOptions, UnlockOptions,
    UnlockTarget, UpdateOptions,
};

/// A reusable configuration object for connecting to an `svn://` server.
///
/// Use [`RaSvnClient::open_session`] to create a connected [`RaSvnSession`].
#[derive(Clone, Debug)]
pub struct RaSvnClient {
    base_url: SvnUrl,
    username: Option<String>,
    password: Option<String>,
    connect_timeout: Duration,
    read_timeout: Duration,
    write_timeout: Duration,
    ra_client: String,
}

/// A connected, stateful session to an `svn://` server.
///
/// A session owns a single TCP connection; operations require `&mut self` and
/// therefore run serially on that connection. Reuse a session if you want to
/// avoid reconnecting/handshaking for each operation.
pub struct RaSvnSession {
    client: RaSvnClient,
    conn: Option<RaSvnConnection>,
    server_info: Option<ServerInfo>,
    allow_reconnect: bool,
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

impl RaSvnClient {
    /// Creates a client configuration for a repository URL and optional credentials.
    ///
    /// Credentials are used when the server offers an auth mechanism supported by
    /// this crate (for example `PLAIN` or `CRAM-MD5`).
    pub fn new(base_url: SvnUrl, username: Option<String>, password: Option<String>) -> Self {
        Self {
            base_url,
            username,
            password,
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(60),
            write_timeout: Duration::from_secs(60),
            ra_client: "prototype-ra_svn".to_string(),
        }
    }

    /// Returns the configured base URL.
    pub fn base_url(&self) -> &SvnUrl {
        &self.base_url
    }

    /// Returns the configured username, if any.
    pub fn username(&self) -> Option<&str> {
        self.username.as_deref()
    }

    /// Returns the configured connect timeout.
    pub fn connect_timeout(&self) -> Duration {
        self.connect_timeout
    }

    /// Returns the configured read timeout.
    pub fn read_timeout(&self) -> Duration {
        self.read_timeout
    }

    /// Returns the configured write timeout.
    pub fn write_timeout(&self) -> Duration {
        self.write_timeout
    }

    /// Returns the configured `ra_client` string sent during handshake.
    pub fn ra_client(&self) -> &str {
        &self.ra_client
    }

    /// Sets the connect timeout.
    #[must_use]
    pub fn with_connect_timeout(mut self, connect_timeout: Duration) -> Self {
        self.connect_timeout = connect_timeout;
        self
    }

    /// Sets the read timeout.
    #[must_use]
    pub fn with_read_timeout(mut self, read_timeout: Duration) -> Self {
        self.read_timeout = read_timeout;
        self
    }

    /// Sets the write timeout.
    #[must_use]
    pub fn with_write_timeout(mut self, write_timeout: Duration) -> Self {
        self.write_timeout = write_timeout;
        self
    }

    /// Sets the `ra_client` string sent to the server during handshake.
    #[must_use]
    pub fn with_ra_client(mut self, ra_client: impl Into<String>) -> Self {
        self.ra_client = ra_client.into();
        self
    }

    /// Opens a new TCP connection, performs the `ra_svn` handshake, and returns a [`RaSvnSession`].
    pub async fn open_session(&self) -> Result<RaSvnSession, SvnError> {
        let mut session = RaSvnSession {
            client: self.clone(),
            conn: None,
            server_info: None,
            allow_reconnect: true,
        };
        session.reconnect().await?;
        Ok(session)
    }

    /// Opens a session over an already connected stream.
    ///
    /// This is useful if you want to provide your own transport (for example a
    /// tunnel or custom proxy). The stream must already be connected to the
    /// same `host:port` as [`RaSvnClient::base_url`].
    ///
    /// Sessions created by this method do **not** auto-reconnect on I/O errors
    /// (because the crate cannot recreate your custom transport). If the stream
    /// is dropped, create a new session yourself.
    pub async fn open_session_with_stream<S>(&self, stream: S) -> Result<RaSvnSession, SvnError>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        let mut session = RaSvnSession {
            client: self.clone(),
            conn: None,
            server_info: None,
            allow_reconnect: false,
        };

        let (conn, server_info) = self.connect_over_stream(stream).await?;
        session.conn = Some(conn);
        session.server_info = Some(server_info);
        Ok(session)
    }

    /// Convenience wrapper for [`RaSvnSession::get_latest_rev`].
    pub async fn get_latest_rev(&self) -> Result<u64, SvnError> {
        let mut session = self.open_session().await?;
        session.get_latest_rev().await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file`].
    pub async fn get_file<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        path: &str,
        rev: u64,
        want_props: bool,
        out: &mut W,
        max_bytes: u64,
    ) -> Result<u64, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_file(path, rev, want_props, out, max_bytes)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file_with_options`].
    pub async fn get_file_with_options<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        path: &str,
        options: &GetFileOptions,
        out: &mut W,
    ) -> Result<GetFileResult, SvnError> {
        let mut session = self.open_session().await?;
        session.get_file_with_options(path, options, out).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file_with_result`].
    pub async fn get_file_with_result<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        path: &str,
        rev: u64,
        want_props: bool,
        out: &mut W,
        max_bytes: u64,
    ) -> Result<GetFileResult, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_file_with_result(path, rev, want_props, out, max_bytes)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::log`].
    pub async fn log(&self, start_rev: u64, end_rev: u64) -> Result<Vec<LogEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.log(start_rev, end_rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::log_with_options`].
    pub async fn log_with_options(&self, options: &LogOptions) -> Result<Vec<LogEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.log_with_options(options).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_dated_rev`].
    pub async fn get_dated_rev(&self, date: &str) -> Result<u64, SvnError> {
        let mut session = self.open_session().await?;
        session.get_dated_rev(date).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_mergeinfo`].
    pub async fn get_mergeinfo(
        &self,
        paths: &[String],
        rev: Option<u64>,
        inherit: MergeInfoInheritance,
        include_descendants: bool,
    ) -> Result<MergeInfoCatalog, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_mergeinfo(paths, rev, inherit, include_descendants)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::get_deleted_rev`].
    pub async fn get_deleted_rev(
        &self,
        path: &str,
        peg_rev: u64,
        end_rev: u64,
    ) -> Result<Option<u64>, SvnError> {
        let mut session = self.open_session().await?;
        session.get_deleted_rev(path, peg_rev, end_rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_locations`].
    pub async fn get_locations(
        &self,
        path: &str,
        peg_rev: u64,
        location_revs: &[u64],
    ) -> Result<Vec<LocationEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.get_locations(path, peg_rev, location_revs).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_location_segments`].
    pub async fn get_location_segments(
        &self,
        path: &str,
        peg_rev: u64,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
    ) -> Result<Vec<LocationSegment>, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_location_segments(path, peg_rev, start_rev, end_rev)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::get_file_revs`].
    pub async fn get_file_revs(
        &self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
    ) -> Result<Vec<crate::FileRev>, SvnError> {
        let mut session = self.open_session().await?;
        session
            .get_file_revs(path, start_rev, end_rev, include_merged_revisions)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::rev_proplist`].
    pub async fn rev_proplist(&self, rev: u64) -> Result<PropertyList, SvnError> {
        let mut session = self.open_session().await?;
        session.rev_proplist(rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::rev_prop`].
    pub async fn rev_prop(&self, rev: u64, name: &str) -> Result<Option<Vec<u8>>, SvnError> {
        let mut session = self.open_session().await?;
        session.rev_prop(rev, name).await
    }

    /// Convenience wrapper for [`RaSvnSession::change_rev_prop`].
    pub async fn change_rev_prop(
        &self,
        rev: u64,
        name: &str,
        value: Option<Vec<u8>>,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.change_rev_prop(rev, name, value).await
    }

    /// Convenience wrapper for [`RaSvnSession::change_rev_prop2`].
    pub async fn change_rev_prop2(
        &self,
        rev: u64,
        name: &str,
        value: Option<Vec<u8>>,
        dont_care: bool,
        previous_value: Option<Vec<u8>>,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session
            .change_rev_prop2(rev, name, value, dont_care, previous_value)
            .await
    }

    /// Convenience wrapper for [`RaSvnSession::proplist`].
    pub async fn proplist(
        &self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Option<PropertyList>, SvnError> {
        let mut session = self.open_session().await?;
        session.proplist(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::propget`].
    pub async fn propget(
        &self,
        path: &str,
        rev: Option<u64>,
        name: &str,
    ) -> Result<Option<Vec<u8>>, SvnError> {
        let mut session = self.open_session().await?;
        session.propget(path, rev, name).await
    }

    /// Convenience wrapper for [`RaSvnSession::inherited_props`].
    pub async fn inherited_props(
        &self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Vec<InheritedProps>, SvnError> {
        let mut session = self.open_session().await?;
        session.inherited_props(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_lock`].
    pub async fn get_lock(&self, path: &str) -> Result<Option<LockDesc>, SvnError> {
        let mut session = self.open_session().await?;
        session.get_lock(path).await
    }

    /// Convenience wrapper for [`RaSvnSession::get_locks`].
    pub async fn get_locks(&self, path: &str, depth: Depth) -> Result<Vec<LockDesc>, SvnError> {
        let mut session = self.open_session().await?;
        session.get_locks(path, depth).await
    }

    /// Convenience wrapper for [`RaSvnSession::lock`].
    pub async fn lock(&self, path: &str, options: &LockOptions) -> Result<LockDesc, SvnError> {
        let mut session = self.open_session().await?;
        session.lock(path, options).await
    }

    /// Convenience wrapper for [`RaSvnSession::lock_many`].
    pub async fn lock_many(
        &self,
        options: &LockManyOptions,
        targets: &[LockTarget],
    ) -> Result<Vec<Result<LockDesc, SvnError>>, SvnError> {
        let mut session = self.open_session().await?;
        session.lock_many(options, targets).await
    }

    /// Convenience wrapper for [`RaSvnSession::unlock`].
    pub async fn unlock(&self, path: &str, options: &UnlockOptions) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.unlock(path, options).await
    }

    /// Convenience wrapper for [`RaSvnSession::unlock_many`].
    pub async fn unlock_many(
        &self,
        options: &UnlockManyOptions,
        targets: &[UnlockTarget],
    ) -> Result<Vec<Result<String, SvnError>>, SvnError> {
        let mut session = self.open_session().await?;
        session.unlock_many(options, targets).await
    }

    /// Convenience wrapper for [`RaSvnSession::commit`].
    pub async fn commit(
        &self,
        options: &CommitOptions,
        commands: &[EditorCommand],
    ) -> Result<CommitInfo, SvnError> {
        let mut session = self.open_session().await?;
        session.commit(options, commands).await
    }

    /// Convenience wrapper for [`RaSvnSession::list_dir`].
    pub async fn list_dir(&self, path: &str, rev: Option<u64>) -> Result<DirListing, SvnError> {
        let mut session = self.open_session().await?;
        session.list_dir(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::list_dir_with_fields`].
    pub async fn list_dir_with_fields(
        &self,
        path: &str,
        rev: Option<u64>,
        fields: &[DirentField],
    ) -> Result<DirListing, SvnError> {
        let mut session = self.open_session().await?;
        session.list_dir_with_fields(path, rev, fields).await
    }

    /// Convenience wrapper for [`RaSvnSession::check_path`].
    pub async fn check_path(&self, path: &str, rev: Option<u64>) -> Result<NodeKind, SvnError> {
        let mut session = self.open_session().await?;
        session.check_path(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::stat`].
    pub async fn stat(&self, path: &str, rev: Option<u64>) -> Result<Option<StatEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.stat(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::list`].
    pub async fn list(
        &self,
        path: &str,
        rev: Option<u64>,
        depth: Depth,
        fields: &[DirentField],
        patterns: Option<&[String]>,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.list(path, rev, depth, fields, patterns).await
    }

    /// Convenience wrapper for [`RaSvnSession::list_with_options`].
    pub async fn list_with_options(
        &self,
        options: &ListOptions,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.list_with_options(options).await
    }

    /// Convenience wrapper for [`RaSvnSession::list_recursive`].
    pub async fn list_recursive(
        &self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let mut session = self.open_session().await?;
        session.list_recursive(path, rev).await
    }

    /// Convenience wrapper for [`RaSvnSession::update`].
    pub async fn update(
        &self,
        options: &UpdateOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.update(options, report, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::switch`].
    pub async fn switch(
        &self,
        options: &SwitchOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.switch(options, report, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::status`].
    pub async fn status(
        &self,
        options: &StatusOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.status(options, report, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::diff`].
    pub async fn diff(
        &self,
        options: &DiffOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.diff(options, report, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::replay`].
    pub async fn replay(
        &self,
        options: &ReplayOptions,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.replay(options, handler).await
    }

    /// Convenience wrapper for [`RaSvnSession::replay_range`].
    pub async fn replay_range(
        &self,
        options: &ReplayRangeOptions,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        let mut session = self.open_session().await?;
        session.replay_range(options, handler).await
    }

    async fn connect(&self) -> Result<(RaSvnConnection, ServerInfo), SvnError> {
        let addr = self.base_url.socket_addr();
        let stream = tokio::time::timeout(self.connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| {
                SvnError::Io(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timed out",
                ))
            })??;
        stream.set_nodelay(true)?;

        #[cfg(feature = "cyrus-sasl")]
        let (local_addrport, remote_addrport) = (
            stream
                .local_addr()
                .ok()
                .map(|addr| format!("{};{}", addr.ip(), addr.port())),
            stream
                .peer_addr()
                .ok()
                .map(|addr| format!("{};{}", addr.ip(), addr.port())),
        );

        let (read, write) = stream.into_split();
        let mut conn = RaSvnConnection::new(
            Box::new(read),
            Box::new(write),
            RaSvnConnectionConfig {
                username: self.username.clone(),
                password: self.password.clone(),
                #[cfg(feature = "cyrus-sasl")]
                host: self.base_url.host.clone(),
                #[cfg(feature = "cyrus-sasl")]
                local_addrport,
                #[cfg(feature = "cyrus-sasl")]
                remote_addrport,
                url: self.base_url.url.clone(),
                ra_client: self.ra_client.clone(),
                read_timeout: self.read_timeout,
                write_timeout: self.write_timeout,
            },
        );
        let server_info = conn.handshake().await?;
        Ok((conn, server_info))
    }

    async fn connect_over_stream<S>(
        &self,
        stream: S,
    ) -> Result<(RaSvnConnection, ServerInfo), SvnError>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
    {
        #[cfg(feature = "cyrus-sasl")]
        let (local_addrport, remote_addrport) = (None, None);

        let (read, write) = tokio::io::split(stream);
        let mut conn = RaSvnConnection::new(
            Box::new(read),
            Box::new(write),
            RaSvnConnectionConfig {
                username: self.username.clone(),
                password: self.password.clone(),
                #[cfg(feature = "cyrus-sasl")]
                host: self.base_url.host.clone(),
                #[cfg(feature = "cyrus-sasl")]
                local_addrport,
                #[cfg(feature = "cyrus-sasl")]
                remote_addrport,
                url: self.base_url.url.clone(),
                ra_client: self.ra_client.clone(),
                read_timeout: self.read_timeout,
                write_timeout: self.write_timeout,
            },
        );
        let server_info = conn.handshake().await?;
        Ok((conn, server_info))
    }
}

impl RaSvnSession {
    /// Returns the [`RaSvnClient`] configuration used to create this session.
    pub fn client(&self) -> &RaSvnClient {
        &self.client
    }

    /// Returns server info collected during handshake, if connected.
    pub fn server_info(&self) -> Option<&ServerInfo> {
        self.server_info.as_ref()
    }

    /// Returns the repository UUID, if available.
    pub fn repos_uuid(&self) -> Option<&str> {
        self.server_info
            .as_ref()
            .map(|info| info.repository.uuid.as_str())
    }

    /// Returns the repository root URL, if available.
    ///
    /// Some older servers may not provide a root URL during handshake.
    pub fn repos_root_url(&self) -> Option<&str> {
        let root = self
            .server_info
            .as_ref()
            .map(|info| info.repository.root_url.as_str())?;
        if root.trim().is_empty() {
            None
        } else {
            Some(root)
        }
    }

    /// Returns `true` if the server advertised the given capability.
    pub fn has_capability(&self, capability: Capability) -> bool {
        let Some(info) = self.server_info.as_ref() else {
            return false;
        };
        let cap = capability.as_wire_word();
        info.server_caps.iter().any(|c| c == cap)
            || info.repository.capabilities.iter().any(|c| c == cap)
    }

    /// Changes the repository URL for this session (server-side `reparent`).
    ///
    /// This is only allowed within the same `host:port` pair.
    pub async fn reparent(&mut self, new_base_url: SvnUrl) -> Result<(), SvnError> {
        if new_base_url.host != self.client.base_url.host
            || new_base_url.port != self.client.base_url.port
        {
            return Err(SvnError::InvalidUrl(
                "reparent requires same host and port".to_string(),
            ));
        }

        let new_url = new_base_url.url.clone();
        self.with_retry("reparent", move |conn| {
            let new_url = new_url.clone();
            Box::pin(async move {
                let response = conn
                    .call(
                        "reparent",
                        SvnItem::List(vec![SvnItem::String(new_url.as_bytes().to_vec())]),
                    )
                    .await?;
                let _ = response.success_params("reparent")?;
                conn.set_session_url(new_url);
                Ok(())
            })
        })
        .await?;

        self.client.base_url = new_base_url;
        Ok(())
    }

    /// Reconnects the underlying TCP connection and performs a new handshake.
    pub async fn reconnect(&mut self) -> Result<(), SvnError> {
        if !self.allow_reconnect {
            return Err(SvnError::Protocol(
                "reconnect not supported for this session".to_string(),
            ));
        }
        let (conn, server_info) = self.client.connect().await?;
        self.conn = Some(conn);
        self.server_info = Some(server_info);
        Ok(())
    }

    async fn ensure_connected(&mut self) -> Result<(), SvnError> {
        if self.conn.is_none() {
            self.reconnect().await?;
        }
        Ok(())
    }

    async fn with_retry<T, F>(&mut self, op: &'static str, mut f: F) -> Result<T, SvnError>
    where
        F: for<'a> FnMut(
            &'a mut RaSvnConnection,
        ) -> Pin<Box<dyn Future<Output = Result<T, SvnError>> + Send + 'a>>,
    {
        let mut attempt = 0usize;
        loop {
            self.ensure_connected().await?;
            let result = {
                let conn = self.conn_mut()?;
                f(conn).await
            };
            match result {
                Ok(v) => return Ok(v),
                Err(err) if attempt == 0 && self.allow_reconnect && is_retryable_error(&err) => {
                    debug!(op, error = %err, "connection lost; reconnecting and retrying");
                    self.reconnect().await?;
                    attempt += 1;
                }
                Err(err) => return Err(err),
            }
        }
    }

    fn conn_mut(&mut self) -> Result<&mut RaSvnConnection, SvnError> {
        self.conn
            .as_mut()
            .ok_or_else(|| SvnError::Protocol("not connected".into()))
    }

    /// Runs `get-latest-rev` and returns the latest (HEAD) revision number.
    pub async fn get_latest_rev(&mut self) -> Result<u64, SvnError> {
        self.with_retry("get-latest-rev", |conn| {
            Box::pin(async move {
                let response = conn
                    .call("get-latest-rev", SvnItem::List(Vec::new()))
                    .await?;
                let params = response.success_params("get-latest-rev")?;
                let rev = params
                    .first()
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| SvnError::Protocol("missing latest rev".into()))?;
                Ok(rev)
            })
        })
        .await
    }

    /// Runs `get-dated-rev` and returns the revision number for a given date.
    ///
    /// The date string is interpreted by the server.
    pub async fn get_dated_rev(&mut self, date: &str) -> Result<u64, SvnError> {
        let date = date.as_bytes().to_vec();
        self.with_retry("get-dated-rev", move |conn| {
            let date = date.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![SvnItem::String(date)]);
                let response = conn.call("get-dated-rev", params).await?;
                let params = response.success_params("get-dated-rev")?;
                let rev = params
                    .first()
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| SvnError::Protocol("missing dated rev".into()))?;
                Ok(rev)
            })
        })
        .await
    }

    /// Runs `get-mergeinfo` for a set of paths.
    pub async fn get_mergeinfo(
        &mut self,
        paths: &[String],
        rev: Option<u64>,
        inherit: MergeInfoInheritance,
        include_descendants: bool,
    ) -> Result<MergeInfoCatalog, SvnError> {
        let paths: Result<Vec<String>, SvnError> =
            paths.iter().map(|p| validate_rel_dir_path(p)).collect();
        let paths = paths?;

        self.with_retry("get-mergeinfo", move |conn| {
            let paths = paths.clone();
            Box::pin(async move {
                let target_paths = SvnItem::List(
                    paths
                        .iter()
                        .map(|p| SvnItem::String(p.as_bytes().to_vec()))
                        .collect(),
                );
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };
                let params = SvnItem::List(vec![
                    target_paths,
                    rev_tuple,
                    SvnItem::Word(inherit.as_word().to_string()),
                    SvnItem::Bool(include_descendants),
                ]);

                let response = conn.call("get-mergeinfo", params).await?;
                let params = response.success_params("get-mergeinfo")?;
                parse_mergeinfo_catalog(params)
            })
        })
        .await
    }

    /// Runs `update` using a client-provided report and consumes the editor drive.
    ///
    /// The report must end with [`ReportCommand::FinishReport`] or
    /// [`ReportCommand::AbortReport`]. Editor events are delivered to `handler`.
    pub async fn update(
        &mut self,
        options: &UpdateOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        require_finish_report(report)?;
        let target = validate_rel_dir_path(&options.target)?;
        let recurse = matches!(options.depth, Depth::Immediates | Depth::Infinity);

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let rev_tuple = match options.rev {
                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                None => SvnItem::List(Vec::new()),
            };
            let params = SvnItem::List(vec![
                rev_tuple,
                SvnItem::String(target.as_bytes().to_vec()),
                SvnItem::Bool(recurse),
                SvnItem::Word(options.depth.as_word().to_string()),
                SvnItem::Bool(options.send_copyfrom_args),
                SvnItem::Bool(options.ignore_ancestry),
            ]);
            conn.send_command("update", params).await?;
            conn.handle_auth_request().await?;
            send_report(conn, report).await?;
            conn.handle_auth_request().await?;
            let _ = drive_editor(conn, Some(handler), false).await?;
            let response = conn.read_command_response().await?;
            let _ = response.success_params("update")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `switch` using a client-provided report and consumes the editor drive.
    ///
    /// The report must end with [`ReportCommand::FinishReport`] or
    /// [`ReportCommand::AbortReport`]. Editor events are delivered to `handler`.
    pub async fn switch(
        &mut self,
        options: &SwitchOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        require_finish_report(report)?;
        let target = validate_rel_dir_path(&options.target)?;
        let recurse = matches!(options.depth, Depth::Immediates | Depth::Infinity);

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let rev_tuple = match options.rev {
                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                None => SvnItem::List(Vec::new()),
            };
            let params = SvnItem::List(vec![
                rev_tuple,
                SvnItem::String(target.as_bytes().to_vec()),
                SvnItem::Bool(recurse),
                SvnItem::String(options.switch_url.as_bytes().to_vec()),
                SvnItem::Word(options.depth.as_word().to_string()),
                SvnItem::Bool(options.send_copyfrom_args),
                SvnItem::Bool(options.ignore_ancestry),
            ]);
            conn.send_command("switch", params).await?;
            conn.handle_auth_request().await?;
            send_report(conn, report).await?;
            conn.handle_auth_request().await?;
            let _ = drive_editor(conn, Some(handler), false).await?;
            let response = conn.read_command_response().await?;
            let _ = response.success_params("switch")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `status` using a client-provided report and consumes the editor drive.
    ///
    /// The report must end with [`ReportCommand::FinishReport`] or
    /// [`ReportCommand::AbortReport`]. Editor events are delivered to `handler`.
    pub async fn status(
        &mut self,
        options: &StatusOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        require_finish_report(report)?;
        let target = validate_rel_dir_path(&options.target)?;
        let recurse = matches!(options.depth, Depth::Immediates | Depth::Infinity);

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let rev_tuple = match options.rev {
                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                None => SvnItem::List(Vec::new()),
            };
            let params = SvnItem::List(vec![
                SvnItem::String(target.as_bytes().to_vec()),
                SvnItem::Bool(recurse),
                rev_tuple,
                SvnItem::Word(options.depth.as_word().to_string()),
            ]);
            conn.send_command("status", params).await?;
            conn.handle_auth_request().await?;
            send_report(conn, report).await?;
            conn.handle_auth_request().await?;
            let _ = drive_editor(conn, Some(handler), false).await?;
            let response = conn.read_command_response().await?;
            let _ = response.success_params("status")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `diff` using a client-provided report and consumes the editor drive.
    ///
    /// The report must end with [`ReportCommand::FinishReport`] or
    /// [`ReportCommand::AbortReport`]. Editor events are delivered to `handler`.
    pub async fn diff(
        &mut self,
        options: &DiffOptions,
        report: &Report,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        require_finish_report(report)?;
        let target = validate_rel_dir_path(&options.target)?;
        let recurse = matches!(options.depth, Depth::Immediates | Depth::Infinity);

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let rev_tuple = match options.rev {
                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                None => SvnItem::List(Vec::new()),
            };
            let params = SvnItem::List(vec![
                rev_tuple,
                SvnItem::String(target.as_bytes().to_vec()),
                SvnItem::Bool(recurse),
                SvnItem::Bool(options.ignore_ancestry),
                SvnItem::String(options.versus_url.as_bytes().to_vec()),
                SvnItem::Bool(options.text_deltas),
                SvnItem::Word(options.depth.as_word().to_string()),
            ]);
            conn.send_command("diff", params).await?;
            conn.handle_auth_request().await?;
            send_report(conn, report).await?;
            conn.handle_auth_request().await?;
            let _ = drive_editor(conn, Some(handler), false).await?;
            let response = conn.read_command_response().await?;
            let _ = response.success_params("diff")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `replay` for a single revision and emits editor events to `handler`.
    pub async fn replay(
        &mut self,
        options: &ReplayOptions,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let params = SvnItem::List(vec![
                SvnItem::Number(options.revision),
                SvnItem::Number(options.low_water_mark),
                SvnItem::Bool(options.send_deltas),
            ]);
            conn.send_command("replay", params).await?;
            conn.handle_auth_request().await?;
            let _ = drive_editor(conn, Some(handler), true).await?;
            let response = conn.read_command_response().await?;
            let _ = response.success_params("replay")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `replay-range` and emits revprops and editor events to `handler`.
    pub async fn replay_range(
        &mut self,
        options: &ReplayRangeOptions,
        handler: &mut dyn EditorEventHandler,
    ) -> Result<(), SvnError> {
        if options.end_rev < options.start_rev {
            return Err(SvnError::Protocol(
                "end_rev must be greater than or equal to start_rev".into(),
            ));
        }

        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let params = SvnItem::List(vec![
                SvnItem::Number(options.start_rev),
                SvnItem::Number(options.end_rev),
                SvnItem::Number(options.low_water_mark),
                SvnItem::Bool(options.send_deltas),
            ]);
            conn.send_command("replay-range", params).await?;
            conn.handle_auth_request().await?;

            for _rev in options.start_rev..=options.end_rev {
                let item = conn.read_item().await?;
                let SvnItem::List(parts) = item else {
                    return Err(SvnError::Protocol("expected revprops tuple".into()));
                };
                if parts.is_empty() {
                    return Err(SvnError::Protocol("empty revprops tuple".into()));
                }

                let word = parts[0]
                    .as_word()
                    .ok_or_else(|| SvnError::Protocol("revprops tuple word not a word".into()))?;
                let props_item = parts
                    .get(1)
                    .cloned()
                    .unwrap_or_else(|| SvnItem::List(Vec::new()));
                let props_list = props_item.as_list().unwrap_or_default();

                match word.as_str() {
                    "revprops" => {
                        let props = parse_proplist(&props_item)?;
                        handler.on_event(EditorEvent::RevProps { props })?;
                    }
                    "failure" => return Err(parse_failure(&props_list)),
                    other => {
                        return Err(SvnError::Protocol(format!(
                            "expected revprops, found '{other}'"
                        )));
                    }
                }

                let aborted = drive_editor(conn, Some(handler), true).await?;
                if aborted {
                    return Err(SvnError::Protocol("error while replaying commit".into()));
                }
            }

            let response = conn.read_command_response().await?;
            let _ = response.success_params("replay-range")?;
            Ok(())
        }
        .await;
        if result.is_err() {
            self.conn = None;
        }
        result
    }

    /// Runs `get-deleted-rev` for a path and returns the deletion revision (if any).
    pub async fn get_deleted_rev(
        &mut self,
        path: &str,
        peg_rev: u64,
        end_rev: u64,
    ) -> Result<Option<u64>, SvnError> {
        let path = validate_rel_path(path)?;
        self.with_retry("get-deleted-rev", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    SvnItem::Number(peg_rev),
                    SvnItem::Number(end_rev),
                ]);

                let response = conn.call("get-deleted-rev", params).await?;
                if response.is_failure() {
                    let message = response.failure_message().to_ascii_lowercase();
                    if message.contains("missing revision") {
                        return Ok(None);
                    }
                    return Err(response.failure("get-deleted-rev"));
                }

                let params = response.success_params("get-deleted-rev")?;
                let deleted = params
                    .first()
                    .and_then(|i| i.as_u64())
                    .ok_or_else(|| SvnError::Protocol("missing deleted rev".into()))?;
                Ok(Some(deleted))
            })
        })
        .await
    }

    /// Runs `get-file`, streaming file contents into `out`.
    ///
    /// Returns the number of bytes written.
    pub async fn get_file<W: tokio::io::AsyncWrite + Unpin>(
        &mut self,
        path: &str,
        rev: u64,
        want_props: bool,
        out: &mut W,
        max_bytes: u64,
    ) -> Result<u64, SvnError> {
        Ok(self
            .get_file_with_result(path, rev, want_props, out, max_bytes)
            .await?
            .bytes_written)
    }

    /// Like [`RaSvnSession::get_file`], but also returns additional metadata.
    pub async fn get_file_with_result<W: tokio::io::AsyncWrite + Unpin>(
        &mut self,
        path: &str,
        rev: u64,
        want_props: bool,
        out: &mut W,
        max_bytes: u64,
    ) -> Result<GetFileResult, SvnError> {
        let options = GetFileOptions {
            rev,
            want_props,
            want_iprops: false,
            max_bytes,
        };
        self.get_file_with_options(path, &options, out).await
    }

    /// Runs `get-file` with a [`GetFileOptions`] builder.
    pub async fn get_file_with_options<W: tokio::io::AsyncWrite + Unpin>(
        &mut self,
        path: &str,
        options: &GetFileOptions,
        out: &mut W,
    ) -> Result<GetFileResult, SvnError> {
        let rev = options.rev;
        let want_props = options.want_props;
        let want_iprops = options.want_iprops;
        let max_bytes = options.max_bytes;

        let path = validate_rel_path(path)?;
        self.ensure_connected().await?;
        let mut attempt = 0usize;
        loop {
            let mut written = 0u64;
            let result = {
                let conn = self.conn_mut()?;

                let params = SvnItem::List(vec![
                    SvnItem::String(path.clone().into_bytes()),
                    SvnItem::List(vec![SvnItem::Number(rev)]),
                    SvnItem::Bool(want_props),
                    SvnItem::Bool(true),
                    // The standard client always sends want-iprops as false and
                    // uses a separate `get-iprops` request (see protocol notes).
                    SvnItem::Bool(false),
                ]);

                conn.send_command("get-file", params).await?;
                conn.handle_auth_request().await?;

                let response = conn.read_command_response().await?;
                let params = response.success_params("get-file")?;
                let meta = parse_get_file_response_params(params)?;

                loop {
                    let item = conn.read_item().await?;
                    let Some(chunk) = item.as_bytes_string() else {
                        return Err(SvnError::Protocol("expected file chunk string".into()));
                    };
                    if chunk.is_empty() {
                        break;
                    }

                    written = written.saturating_add(chunk.len() as u64);
                    if written > max_bytes {
                        return Err(SvnError::Protocol(format!(
                            "downloaded file exceeds limit {max_bytes}"
                        )));
                    }
                    out.write_all(&chunk).await?;
                }

                let post = conn.read_command_response().await?;
                if post.is_failure() {
                    return Err(post.failure("get-file"));
                }

                Ok(GetFileResult {
                    rev: meta.rev,
                    checksum: meta.checksum,
                    props: meta.props,
                    inherited_props: meta.inherited_props,
                    bytes_written: written,
                })
            };

            match result {
                Ok(mut result) => {
                    if want_iprops && self.has_capability(Capability::InheritedProps) {
                        result.inherited_props =
                            self.inherited_props(&path, Some(result.rev)).await?;
                    }
                    return Ok(result);
                }
                Err(err) if attempt == 0 && written == 0 && is_retryable_error(&err) => {
                    debug!("get-file connection lost before data; reconnecting and retrying");
                    self.reconnect().await?;
                    attempt += 1;
                }
                Err(err) => {
                    if should_drop_connection(&err) {
                        self.conn = None;
                    }
                    return Err(err);
                }
            }
        }
    }

    /// Runs `rev-proplist` and returns all revision properties for `rev`.
    pub async fn rev_proplist(&mut self, rev: u64) -> Result<PropertyList, SvnError> {
        self.with_retry("rev-proplist", move |conn| {
            Box::pin(async move {
                let response = conn
                    .call("rev-proplist", SvnItem::List(vec![SvnItem::Number(rev)]))
                    .await?;
                let params = response.success_params("rev-proplist")?;
                let proplist = params.first().ok_or_else(|| {
                    SvnError::Protocol("rev-proplist response missing proplist".into())
                })?;
                parse_proplist(proplist)
            })
        })
        .await
    }

    /// Runs `rev-prop` and returns a single revision property value.
    pub async fn rev_prop(&mut self, rev: u64, name: &str) -> Result<Option<Vec<u8>>, SvnError> {
        let name = name.as_bytes().to_vec();
        self.with_retry("rev-prop", move |conn| {
            let name = name.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![SvnItem::Number(rev), SvnItem::String(name)]);
                let response = conn.call("rev-prop", params).await?;
                let params = response.success_params("rev-prop")?;
                let Some(value_tuple) = params.first() else {
                    return Ok(None);
                };
                let items = value_tuple
                    .as_list()
                    .ok_or_else(|| SvnError::Protocol("rev-prop value tuple not a list".into()))?;
                let Some(value) = items.first() else {
                    return Ok(None);
                };
                let value = value
                    .as_bytes_string()
                    .ok_or_else(|| SvnError::Protocol("rev-prop value not a string".into()))?;
                Ok(Some(value))
            })
        })
        .await
    }

    /// Runs `change-rev-prop` to set or delete a revision property.
    pub async fn change_rev_prop(
        &mut self,
        rev: u64,
        name: &str,
        value: Option<Vec<u8>>,
    ) -> Result<(), SvnError> {
        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;
            let name = name.as_bytes().to_vec();
            let mut items = vec![SvnItem::Number(rev), SvnItem::String(name)];
            if let Some(value) = value {
                items.push(SvnItem::String(value));
            }

            let response = conn.call("change-rev-prop", SvnItem::List(items)).await?;
            let _ = response.success_params("change-rev-prop")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `change-rev-prop2` to atomically set or delete a revision property.
    ///
    /// This requires the server to support `atomic-revprops`.
    pub async fn change_rev_prop2(
        &mut self,
        rev: u64,
        name: &str,
        value: Option<Vec<u8>>,
        dont_care: bool,
        previous_value: Option<Vec<u8>>,
    ) -> Result<(), SvnError> {
        if dont_care && previous_value.is_some() {
            return Err(SvnError::Protocol(
                "change-rev-prop2 previous_value must be None when dont_care is true".into(),
            ));
        }

        self.ensure_connected().await?;
        if self.server_info.is_some() {
            let conn = self.conn.as_ref().ok_or_else(|| {
                SvnError::Protocol("change-rev-prop2 requires a connected session".into())
            })?;
            if !conn.server_has_cap("atomic-revprops") {
                return Err(SvnError::Protocol(
                    "server does not support atomic revision property changes".into(),
                ));
            }
        }
        let result = async {
            let conn = self.conn_mut()?;

            let name = name.as_bytes().to_vec();
            let value_tuple = match value {
                Some(value) => SvnItem::List(vec![SvnItem::String(value)]),
                None => SvnItem::List(Vec::new()),
            };

            let mut cond_items = vec![SvnItem::Bool(dont_care)];
            if let Some(previous_value) = previous_value {
                cond_items.push(SvnItem::String(previous_value));
            }
            let cond_tuple = SvnItem::List(cond_items);

            let params = SvnItem::List(vec![
                SvnItem::Number(rev),
                SvnItem::String(name),
                value_tuple,
                cond_tuple,
            ]);

            let response = conn.call("change-rev-prop2", params).await?;
            let _ = response.success_params("change-rev-prop2")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Returns the node properties for a file or directory path.
    ///
    /// This method issues `get-file`/`get-dir` internally depending on the node
    /// kind. Returns `Ok(None)` if the node does not exist.
    pub async fn proplist(
        &mut self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Option<PropertyList>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        let kind = self.check_path(&path, rev).await?;
        match kind {
            NodeKind::None => Ok(None),
            NodeKind::Unknown => Err(SvnError::Protocol("node kind unknown".into())),
            NodeKind::File => {
                let path = validate_rel_path(&path)?;
                let props = self
                    .with_retry("get-file-proplist", move |conn| {
                        let path = path.clone();
                        Box::pin(async move {
                            let rev_tuple = match rev {
                                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                                None => SvnItem::List(Vec::new()),
                            };

                            let params = SvnItem::List(vec![
                                SvnItem::String(path.as_bytes().to_vec()),
                                rev_tuple,
                                SvnItem::Bool(true),  // want-props
                                SvnItem::Bool(false), // want-contents
                                // The standard client always sends want-iprops as false and
                                // uses a separate `get-iprops` request (see protocol notes).
                                SvnItem::Bool(false),
                            ]);

                            let response = conn.call("get-file", params).await?;
                            let params = response.success_params("get-file")?;
                            let meta = parse_get_file_response_params(params)?;
                            Ok(meta.props)
                        })
                    })
                    .await?;
                Ok(Some(props))
            }
            NodeKind::Dir => {
                let props = self
                    .with_retry("get-dir-proplist", move |conn| {
                        let path = path.clone();
                        Box::pin(async move {
                            let rev_tuple = match rev {
                                Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                                None => SvnItem::List(Vec::new()),
                            };

                            let params = SvnItem::List(vec![
                                SvnItem::String(path.as_bytes().to_vec()),
                                rev_tuple,
                                SvnItem::Bool(true),  // want-props
                                SvnItem::Bool(false), // want-contents
                                SvnItem::List(Vec::new()),
                                // The standard client always sends want-iprops as false and
                                // uses a separate `get-iprops` request (see protocol notes).
                                SvnItem::Bool(false),
                            ]);

                            let response = conn.call("get-dir", params).await?;
                            let params = response.success_params("get-dir")?;
                            if params.len() < 2 {
                                return Err(SvnError::Protocol(
                                    "get-dir response missing props".into(),
                                ));
                            }
                            parse_proplist(&params[1])
                        })
                    })
                    .await?;
                Ok(Some(props))
            }
        }
    }

    /// Returns a single property value for a file or directory path.
    pub async fn propget(
        &mut self,
        path: &str,
        rev: Option<u64>,
        name: &str,
    ) -> Result<Option<Vec<u8>>, SvnError> {
        let Some(props) = self.proplist(path, rev).await? else {
            return Ok(None);
        };
        Ok(props.get(name).cloned())
    }

    /// Runs `get-iprops` and returns inherited properties for a path.
    pub async fn inherited_props(
        &mut self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Vec<InheritedProps>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        self.with_retry("get-iprops", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params =
                    SvnItem::List(vec![SvnItem::String(path.as_bytes().to_vec()), rev_tuple]);
                let response = conn.call("get-iprops", params).await?;
                let params = response.success_params("get-iprops")?;
                let iproplist = params.first().ok_or_else(|| {
                    SvnError::Protocol("get-iprops response missing iproplist".into())
                })?;
                parse_iproplist(iproplist)
            })
        })
        .await
    }

    /// Runs `get-locations` and returns path locations for the requested revisions.
    pub async fn get_locations(
        &mut self,
        path: &str,
        peg_rev: u64,
        location_revs: &[u64],
    ) -> Result<Vec<LocationEntry>, SvnError> {
        let path = validate_rel_path(path)?;
        let revs = location_revs.to_vec();
        self.with_retry("get-locations", move |conn| {
            let path = path.clone();
            let revs = revs.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    SvnItem::Number(peg_rev),
                    SvnItem::List(revs.into_iter().map(SvnItem::Number).collect()),
                ]);

                conn.send_command("get-locations", params).await?;
                conn.handle_auth_request().await?;

                let mut out = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(_) => out.push(parse_location_entry(item)?),
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected location entry item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                if response.is_failure() {
                    return Err(response.failure("get-locations"));
                }
                Ok(out)
            })
        })
        .await
    }

    /// Runs `get-location-segments` and returns location segments for a path.
    pub async fn get_location_segments(
        &mut self,
        path: &str,
        peg_rev: u64,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
    ) -> Result<Vec<LocationSegment>, SvnError> {
        let path = validate_rel_path(path)?;
        self.with_retry("get-location-segments", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let peg_tuple = SvnItem::List(vec![SvnItem::Number(peg_rev)]);
                let start_tuple = match start_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };
                let end_tuple = match end_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    peg_tuple,
                    start_tuple,
                    end_tuple,
                ]);

                conn.send_command("get-location-segments", params).await?;
                conn.handle_auth_request().await?;

                let mut out = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(_) => out.push(parse_location_segment(item)?),
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected location segment item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                response.ensure_success("get-location-segments")?;
                Ok(out)
            })
        })
        .await
    }

    /// Runs `get-file-revs` and returns file revisions (including delta chunks).
    pub async fn get_file_revs(
        &mut self,
        path: &str,
        start_rev: Option<u64>,
        end_rev: Option<u64>,
        include_merged_revisions: bool,
    ) -> Result<Vec<crate::FileRev>, SvnError> {
        let path = validate_rel_path(path)?;
        self.with_retry("get-file-revs", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let start_tuple = match start_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };
                let end_tuple = match end_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    start_tuple,
                    end_tuple,
                    SvnItem::Bool(include_merged_revisions),
                ]);

                conn.send_command("get-file-revs", params).await?;
                conn.handle_auth_request().await?;

                let mut out = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(_) => {
                            let mut rev_entry = parse_file_rev_entry(item)?;
                            loop {
                                let chunk = conn.read_item().await?;
                                let Some(bytes) = chunk.as_bytes_string() else {
                                    return Err(SvnError::Protocol(
                                        "file-rev delta chunk not a string".into(),
                                    ));
                                };
                                if bytes.is_empty() {
                                    break;
                                }
                                rev_entry.delta_chunks.push(bytes);
                            }
                            out.push(rev_entry);
                        }
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected file-rev entry item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                response.ensure_success("get-file-revs")?;
                if out.is_empty() {
                    return Err(SvnError::Protocol(
                        "The get-file-revs command didn't return any revisions".into(),
                    ));
                }
                Ok(out)
            })
        })
        .await
    }

    /// Runs `get-lock` and returns the lock for `path`, if any.
    pub async fn get_lock(&mut self, path: &str) -> Result<Option<LockDesc>, SvnError> {
        let path = validate_rel_path(path)?;
        self.with_retry("get-lock", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let response = conn
                    .call(
                        "get-lock",
                        SvnItem::List(vec![SvnItem::String(path.into_bytes())]),
                    )
                    .await?;
                let params = response.success_params("get-lock")?;
                let Some(tuple) = params.first() else {
                    return Ok(None);
                };
                let list = tuple
                    .as_list()
                    .ok_or_else(|| SvnError::Protocol("get-lock tuple not a list".into()))?;
                let Some(lock_item) = list.first() else {
                    return Ok(None);
                };
                Ok(Some(parse_lockdesc(lock_item)?))
            })
        })
        .await
    }

    /// Runs `get-locks` and returns all locks under a directory.
    pub async fn get_locks(&mut self, path: &str, depth: Depth) -> Result<Vec<LockDesc>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        self.with_retry("get-locks", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    SvnItem::List(vec![SvnItem::Word(depth.as_word().to_string())]),
                ]);
                let response = conn.call("get-locks", params).await?;
                let params = response.success_params("get-locks")?;
                let locks_list = params
                    .first()
                    .and_then(|i| i.as_list())
                    .ok_or_else(|| SvnError::Protocol("get-locks response not a list".into()))?;
                let mut out = Vec::new();
                for item in locks_list {
                    out.push(parse_lockdesc(&item)?);
                }
                Ok(out)
            })
        })
        .await
    }

    /// Runs `lock` to acquire a lock for a single path.
    pub async fn lock(&mut self, path: &str, options: &LockOptions) -> Result<LockDesc, SvnError> {
        let path = validate_rel_path(path)?;
        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;

            let comment_tuple = match &options.comment {
                Some(comment) => SvnItem::List(vec![SvnItem::String(comment.as_bytes().to_vec())]),
                None => SvnItem::List(Vec::new()),
            };
            let current_rev_tuple = match options.current_rev {
                Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                None => SvnItem::List(Vec::new()),
            };

            let params = SvnItem::List(vec![
                SvnItem::String(path.as_bytes().to_vec()),
                comment_tuple,
                SvnItem::Bool(options.steal_lock),
                current_rev_tuple,
            ]);

            let response = conn.call("lock", params).await?;
            let params = response.success_params("lock")?;
            let lock_item = params
                .first()
                .ok_or_else(|| SvnError::Protocol("lock response missing lockdesc".into()))?;
            parse_lockdesc(lock_item)
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `lock-many` and returns a per-target result vector.
    ///
    /// The outer `Result` represents transport/protocol failures; each inner
    /// `Result` corresponds to one target.
    pub async fn lock_many(
        &mut self,
        options: &LockManyOptions,
        targets: &[LockTarget],
    ) -> Result<Vec<Result<LockDesc, SvnError>>, SvnError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_connected().await?;
        let result = async {
            let maybe_out: Option<Vec<Result<LockDesc, SvnError>>> = {
                let conn = self.conn_mut()?;

                let comment_tuple = match &options.comment {
                    Some(comment) => {
                        SvnItem::List(vec![SvnItem::String(comment.as_bytes().to_vec())])
                    }
                    None => SvnItem::List(Vec::new()),
                };

                let mut targets_items = Vec::with_capacity(targets.len());
                for target in targets {
                    let path = validate_rel_path(&target.path)?;
                    let rev_tuple = match target.current_rev {
                        Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                        None => SvnItem::List(Vec::new()),
                    };
                    targets_items.push(SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        rev_tuple,
                    ]));
                }

                let params = SvnItem::List(vec![
                    comment_tuple,
                    SvnItem::Bool(options.steal_lock),
                    SvnItem::List(targets_items),
                ]);

                conn.send_command("lock-many", params).await?;
                conn.handle_auth_request().await?;

                let mut out: Vec<Result<LockDesc, SvnError>> = Vec::with_capacity(targets.len());
                let mut unsupported = false;
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(items) => {
                            let status =
                                items.first().and_then(|i| i.as_word()).ok_or_else(|| {
                                    SvnError::Protocol("lock-many status not a word".into())
                                })?;
                            let params =
                                items.get(1).and_then(|i| i.as_list()).ok_or_else(|| {
                                    SvnError::Protocol("lock-many params not a list".into())
                                })?;
                            match status.as_str() {
                                "success" => {
                                    let lock_item = params.first().ok_or_else(|| {
                                        SvnError::Protocol(
                                            "lock-many success missing lockdesc".into(),
                                        )
                                    })?;
                                    out.push(parse_lockdesc(lock_item));
                                }
                                "failure" => {
                                    let err = parse_failure(&params);
                                    if out.is_empty() && is_unknown_command_error(&err) {
                                        unsupported = true;
                                        break;
                                    }
                                    out.push(Err(err));
                                }
                                other => {
                                    return Err(SvnError::Protocol(format!(
                                        "unexpected lock-many status: {other}"
                                    )));
                                }
                            }
                        }
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected lock-many item: {}",
                                other.kind()
                            )));
                        }
                    }
                    if out.len() > targets.len() {
                        return Err(SvnError::Protocol(
                            "lock-many returned more results than targets".into(),
                        ));
                    }
                }

                if unsupported {
                    Ok::<Option<Vec<Result<LockDesc, SvnError>>>, SvnError>(None)
                } else {
                    let response = conn.read_command_response().await?;
                    response.ensure_success("lock-many")?;
                    if out.len() != targets.len() {
                        return Err(SvnError::Protocol(format!(
                            "lock-many returned {} results for {} targets",
                            out.len(),
                            targets.len()
                        )));
                    }

                    Ok(Some(out))
                }
            }?;

            if let Some(out) = maybe_out {
                return Ok(out);
            }

            let mut out: Vec<Result<LockDesc, SvnError>> = Vec::with_capacity(targets.len());
            for target in targets {
                let opts = LockOptions {
                    comment: options.comment.clone(),
                    steal_lock: options.steal_lock,
                    current_rev: target.current_rev,
                };
                out.push(self.lock(&target.path, &opts).await);
            }
            Ok(out)
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `unlock` to release a lock for a single path.
    pub async fn unlock(&mut self, path: &str, options: &UnlockOptions) -> Result<(), SvnError> {
        let path = validate_rel_path(path)?;
        self.ensure_connected().await?;
        let result = async {
            let conn = self.conn_mut()?;

            let token_tuple = match &options.token {
                Some(token) => SvnItem::List(vec![SvnItem::String(token.as_bytes().to_vec())]),
                None => SvnItem::List(Vec::new()),
            };

            let params = SvnItem::List(vec![
                SvnItem::String(path.as_bytes().to_vec()),
                token_tuple,
                SvnItem::Bool(options.break_lock),
            ]);

            let response = conn.call("unlock", params).await?;
            let _ = response.success_params("unlock")?;
            Ok(())
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `unlock-many` and returns a per-target result vector.
    ///
    /// The outer `Result` represents transport/protocol failures; each inner
    /// `Result` corresponds to one target.
    pub async fn unlock_many(
        &mut self,
        options: &UnlockManyOptions,
        targets: &[UnlockTarget],
    ) -> Result<Vec<Result<String, SvnError>>, SvnError> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        self.ensure_connected().await?;
        let result = async {
            let maybe_out: Option<Vec<Result<String, SvnError>>> = {
                let conn = self.conn_mut()?;

                let mut targets_items = Vec::with_capacity(targets.len());
                for target in targets {
                    let path = validate_rel_path(&target.path)?;
                    let token_tuple = match &target.token {
                        Some(token) => {
                            SvnItem::List(vec![SvnItem::String(token.as_bytes().to_vec())])
                        }
                        None => SvnItem::List(Vec::new()),
                    };
                    targets_items.push(SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        token_tuple,
                    ]));
                }

                let params = SvnItem::List(vec![
                    SvnItem::Bool(options.break_lock),
                    SvnItem::List(targets_items),
                ]);

                conn.send_command("unlock-many", params).await?;
                conn.handle_auth_request().await?;

                let mut out: Vec<Result<String, SvnError>> = Vec::with_capacity(targets.len());
                let mut unsupported = false;
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(items) => {
                            let status =
                                items.first().and_then(|i| i.as_word()).ok_or_else(|| {
                                    SvnError::Protocol("unlock-many status not a word".into())
                                })?;
                            let params =
                                items.get(1).and_then(|i| i.as_list()).ok_or_else(|| {
                                    SvnError::Protocol("unlock-many params not a list".into())
                                })?;
                            match status.as_str() {
                                "success" => {
                                    let path = params
                                        .first()
                                        .and_then(|i| i.as_string())
                                        .ok_or_else(|| {
                                            SvnError::Protocol(
                                                "unlock-many success missing path".into(),
                                            )
                                        })?
                                        .trim_start_matches('/')
                                        .to_string();
                                    out.push(Ok(path));
                                }
                                "failure" => {
                                    let err = parse_failure(&params);
                                    if out.is_empty() && is_unknown_command_error(&err) {
                                        unsupported = true;
                                        break;
                                    }
                                    out.push(Err(err));
                                }
                                other => {
                                    return Err(SvnError::Protocol(format!(
                                        "unexpected unlock-many status: {other}"
                                    )));
                                }
                            }
                        }
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected unlock-many item: {}",
                                other.kind()
                            )));
                        }
                    }
                    if out.len() > targets.len() {
                        return Err(SvnError::Protocol(
                            "unlock-many returned more results than targets".into(),
                        ));
                    }
                }

                if unsupported {
                    Ok::<Option<Vec<Result<String, SvnError>>>, SvnError>(None)
                } else {
                    let response = conn.read_command_response().await?;
                    response.ensure_success("unlock-many")?;
                    if out.len() != targets.len() {
                        return Err(SvnError::Protocol(format!(
                            "unlock-many returned {} results for {} targets",
                            out.len(),
                            targets.len()
                        )));
                    }

                    Ok(Some(out))
                }
            }?;

            if let Some(out) = maybe_out {
                return Ok(out);
            }

            let mut out: Vec<Result<String, SvnError>> = Vec::with_capacity(targets.len());
            for target in targets {
                let path = validate_rel_path(&target.path)?;
                let opts = UnlockOptions {
                    token: target.token.clone(),
                    break_lock: options.break_lock,
                };
                match self.unlock(&path, &opts).await {
                    Ok(()) => out.push(Ok(path)),
                    Err(err) => out.push(Err(err)),
                }
            }
            Ok(out)
        }
        .await;
        if let Err(err) = &result
            && should_drop_connection(err)
        {
            self.conn = None;
        }
        result
    }

    /// Runs `commit` using a low-level editor command sequence.
    ///
    /// This is a low-level API: `commands` must form a valid edit and must end
    /// with [`EditorCommand::CloseEdit`].
    pub async fn commit(
        &mut self,
        options: &CommitOptions,
        commands: &[EditorCommand],
    ) -> Result<CommitInfo, SvnError> {
        if commands.is_empty() {
            return Err(SvnError::Protocol(
                "commit requires at least a close-edit command".into(),
            ));
        }
        if !matches!(commands.last(), Some(EditorCommand::CloseEdit)) {
            return Err(SvnError::Protocol(
                "commit editor commands must end with close-edit".into(),
            ));
        }
        if commands
            .iter()
            .take(commands.len().saturating_sub(1))
            .any(|c| matches!(c, EditorCommand::CloseEdit | EditorCommand::AbortEdit))
        {
            return Err(SvnError::Protocol(
                "commit editor commands may only close or abort at the end".into(),
            ));
        }
        if commands
            .iter()
            .any(|c| matches!(c, EditorCommand::AbortEdit))
        {
            return Err(SvnError::Protocol(
                "commit does not support user-supplied abort-edit".into(),
            ));
        }

        self.ensure_connected().await?;
        let result = async {
            let ra_client = self.client.ra_client.clone();
            let conn = self.conn_mut()?;
            let server_supports_revprops = conn.server_has_cap("commit-revprops");
            let server_supports_ephemeral_txnprops = conn.server_has_cap("ephemeral-txnprops");
            let has_non_log_revprops = options.rev_props.keys().any(|k| k != "svn:log");
            if !server_supports_revprops && has_non_log_revprops {
                return Err(SvnError::Protocol(
                    "server does not support setting revision properties during commit".into(),
                ));
            }

            let mut rev_props = options.rev_props.clone();
            rev_props.insert(
                "svn:log".to_string(),
                options.log_message.as_bytes().to_vec(),
            );
            if server_supports_revprops && server_supports_ephemeral_txnprops {
                let compat = txn_client_compat_version(&ra_client);
                rev_props.insert(
                    "svn:txn-client-compat-version".to_string(),
                    compat.as_bytes().to_vec(),
                );
                rev_props.insert(
                    "svn:txn-user-agent".to_string(),
                    ra_client.as_bytes().to_vec(),
                );
            }

            let mut lock_tokens_items = Vec::with_capacity(options.lock_tokens.len());
            for lock_token in &options.lock_tokens {
                let path = validate_rel_path(&lock_token.path)?;
                lock_tokens_items.push(SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    SvnItem::String(lock_token.token.as_bytes().to_vec()),
                ]));
            }

            let params = SvnItem::List(vec![
                SvnItem::String(options.log_message.as_bytes().to_vec()),
                SvnItem::List(lock_tokens_items),
                SvnItem::Bool(options.keep_locks),
                encode_proplist(&rev_props),
            ]);

            let response = conn.call("commit", params).await?;
            let _ = response.success_params("commit")?;

            const MAX_BATCH_BYTES: usize = 256 * 1024;
            const MAX_COMMANDS_PER_BATCH: usize = 32;

            let mut batch = Vec::new();
            let mut since_poll = 0usize;
            for command in commands {
                if since_poll == 0 {
                    check_for_edit_status(conn).await?;
                }
                encode_editor_command(command, &mut batch)?;
                since_poll += 1;
                if since_poll >= MAX_COMMANDS_PER_BATCH || batch.len() >= MAX_BATCH_BYTES {
                    conn.write_wire_bytes(&batch).await?;
                    batch.clear();
                    since_poll = 0;
                }
            }
            if !batch.is_empty() {
                conn.write_wire_bytes(&batch).await?;
            }

            let response = conn.read_command_response().await?;
            response.ensure_success("commit")?;

            conn.handle_auth_request().await?;
            let item = conn.read_item().await?;
            parse_commit_info(&item)
        }
        .await;
        if result.is_err() {
            self.conn = None;
        }
        result
    }

    /// Convenience wrapper for [`RaSvnSession::log_with_options`] over a revision range.
    pub async fn log(&mut self, start_rev: u64, end_rev: u64) -> Result<Vec<LogEntry>, SvnError> {
        let options = LogOptions::between(start_rev, end_rev);
        self.log_with_options(&options).await
    }

    /// Runs `log` with a [`LogOptions`] builder.
    pub async fn log_with_options(
        &mut self,
        options: &LogOptions,
    ) -> Result<Vec<LogEntry>, SvnError> {
        let target_paths = options.target_paths.clone();
        let start_rev = options.start_rev;
        let end_rev = options.end_rev;
        let changed_paths = options.changed_paths;
        let strict_node = options.strict_node;
        let limit = options.limit;
        let include_merged_revisions = options.include_merged_revisions;
        let revprops = options.revprops.clone();

        self.with_retry("log", move |conn| {
            let target_paths = target_paths.clone();
            let revprops = revprops.clone();
            Box::pin(async move {
                let target_paths = SvnItem::List(
                    target_paths
                        .into_iter()
                        .map(|p| SvnItem::String(p.into_bytes()))
                        .collect(),
                );
                let start_rev_tuple = match start_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };
                let end_rev_tuple = match end_rev {
                    Some(rev) => SvnItem::List(vec![SvnItem::Number(rev)]),
                    None => SvnItem::List(Vec::new()),
                };

                let (want_author, want_date, want_message, want_custom_revprops) = match &revprops {
                    LogRevProps::All => (true, true, true, true),
                    LogRevProps::Custom(names) => {
                        let mut want_author = false;
                        let mut want_date = false;
                        let mut want_message = false;
                        let mut want_custom_revprops = false;
                        for name in names {
                            match name.as_str() {
                                "svn:author" => want_author = true,
                                "svn:date" => want_date = true,
                                "svn:log" => want_message = true,
                                _ => want_custom_revprops = true,
                            }
                        }
                        (want_author, want_date, want_message, want_custom_revprops)
                    }
                };

                let mut params_items = vec![
                    target_paths,
                    start_rev_tuple,
                    end_rev_tuple,
                    SvnItem::Bool(changed_paths),
                    SvnItem::Bool(strict_node),
                    SvnItem::Number(limit),
                    SvnItem::Bool(include_merged_revisions),
                ];
                match &revprops {
                    LogRevProps::All => {
                        params_items.push(SvnItem::Word("all-revprops".to_string()));
                    }
                    LogRevProps::Custom(revprops) => {
                        params_items.push(SvnItem::Word("revprops".to_string()));
                        params_items.push(SvnItem::List(
                            revprops
                                .iter()
                                .map(|p| SvnItem::String(p.as_bytes().to_vec()))
                                .collect(),
                        ));
                    }
                }

                let params = SvnItem::List(params_items);

                conn.send_command("log", params).await?;
                conn.handle_auth_request().await?;

                let mut entries = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(items) => {
                            let mut entry = parse_log_entry(items, want_custom_revprops)?;
                            if want_author && let Some(author) = entry.author.as_deref() {
                                entry
                                    .rev_props
                                    .insert("svn:author".to_string(), author.as_bytes().to_vec());
                            }
                            if want_date && let Some(date) = entry.date.as_deref() {
                                entry
                                    .rev_props
                                    .insert("svn:date".to_string(), date.as_bytes().to_vec());
                            }
                            if want_message && let Some(message) = entry.message.as_deref() {
                                entry
                                    .rev_props
                                    .insert("svn:log".to_string(), message.as_bytes().to_vec());
                            }
                            entries.push(entry);
                        }
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected log entry item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                response.ensure_success("log")?;
                Ok(entries)
            })
        })
        .await
    }

    /// Runs `get-dir` and returns a directory listing.
    pub async fn list_dir(&mut self, path: &str, rev: Option<u64>) -> Result<DirListing, SvnError> {
        let path = validate_rel_dir_path(path)?;
        self.with_retry("get-dir", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let fields = [
                    DirentField::Kind,
                    DirentField::Size,
                    DirentField::HasProps,
                    DirentField::CreatedRev,
                    DirentField::Time,
                    DirentField::LastAuthor,
                ];
                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    rev_tuple,
                    SvnItem::Bool(false), // want-props
                    SvnItem::Bool(true),  // want-contents
                    SvnItem::List(
                        fields
                            .iter()
                            .map(|f| SvnItem::Word(f.as_word().to_string()))
                            .collect(),
                    ),
                    SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
                ]);

                let response = conn.call("get-dir", params).await?;
                let params = response.success_params("get-dir")?;
                parse_get_dir_listing(&path, params)
            })
        })
        .await
    }

    /// Runs `get-dir` and requests specific directory entry fields.
    pub async fn list_dir_with_fields(
        &mut self,
        path: &str,
        rev: Option<u64>,
        fields: &[DirentField],
    ) -> Result<DirListing, SvnError> {
        let path = validate_rel_dir_path(path)?;
        let fields = if fields.is_empty() {
            vec![DirentField::Kind]
        } else {
            fields.to_vec()
        };

        self.with_retry("get-dir", move |conn| {
            let path = path.clone();
            let fields = fields.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params = SvnItem::List(vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    rev_tuple,
                    SvnItem::Bool(false), // want-props
                    SvnItem::Bool(true),  // want-contents
                    SvnItem::List(
                        fields
                            .iter()
                            .map(|f| SvnItem::Word(f.as_word().to_string()))
                            .collect(),
                    ),
                    SvnItem::Bool(false), // want-iprops
                ]);

                let response = conn.call("get-dir", params).await?;
                let params = response.success_params("get-dir")?;
                parse_get_dir_listing(&path, params)
            })
        })
        .await
    }

    /// Runs `check-path` and returns the node kind at `path` and `rev`.
    pub async fn check_path(&mut self, path: &str, rev: Option<u64>) -> Result<NodeKind, SvnError> {
        let path = validate_rel_dir_path(path)?;
        self.with_retry("check-path", move |conn| {
            let path = path.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let params =
                    SvnItem::List(vec![SvnItem::String(path.as_bytes().to_vec()), rev_tuple]);

                let response = conn.call("check-path", params).await?;
                let params = response.success_params("check-path")?;
                let kind_word = params
                    .first()
                    .and_then(opt_tuple_wordish)
                    .ok_or_else(|| SvnError::Protocol("check-path response missing kind".into()))?;
                Ok(NodeKind::from_word(&kind_word))
            })
        })
        .await
    }

    /// Runs `stat` and returns basic information about a node.
    ///
    /// Returns `Ok(None)` if the path does not exist.
    pub async fn stat(
        &mut self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Option<StatEntry>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        let path_for_request = path.clone();
        let stat = self
            .with_retry("stat", move |conn| {
                let path = path_for_request.clone();
                Box::pin(async move {
                    let rev_tuple = match rev {
                        Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                        None => SvnItem::List(Vec::new()),
                    };

                    let params =
                        SvnItem::List(vec![SvnItem::String(path.as_bytes().to_vec()), rev_tuple]);

                    let response = conn.call("stat", params).await?;
                    let params = response.success_params("stat")?;
                    Ok(parse_stat_params(params))
                })
            })
            .await?;

        if stat.is_some() {
            return Ok(stat);
        }

        // Some svnserve versions include extra fields / nesting for `stat`. Fall back to
        // `check-path` so callers can still detect file/dir.
        let kind = self.check_path(&path, rev).await?;
        if kind == NodeKind::None {
            return Ok(None);
        }
        Ok(Some(StatEntry {
            kind,
            size: None,
            has_props: None,
            created_rev: None,
            created_date: None,
            last_author: None,
        }))
    }

    /// Runs [`RaSvnSession::list`] using a [`ListOptions`] builder.
    pub async fn list_with_options(
        &mut self,
        options: &ListOptions,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let patterns = if options.patterns.is_empty() {
            None
        } else {
            Some(options.patterns.as_slice())
        };
        self.list(
            &options.path,
            options.rev,
            options.depth,
            &options.fields,
            patterns,
        )
        .await
    }

    /// Runs `list` (server capability) and returns directory entries.
    pub async fn list(
        &mut self,
        path: &str,
        rev: Option<u64>,
        depth: Depth,
        fields: &[DirentField],
        patterns: Option<&[String]>,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let path = validate_rel_dir_path(path)?;
        let fields = if fields.is_empty() {
            vec![DirentField::Kind]
        } else {
            fields.to_vec()
        };
        let patterns = patterns.map(ToOwned::to_owned);

        self.with_retry("list", move |conn| {
            let path = path.clone();
            let fields = fields.clone();
            let patterns = patterns.clone();
            Box::pin(async move {
                let rev_tuple = match rev {
                    Some(r) => SvnItem::List(vec![SvnItem::Number(r)]),
                    None => SvnItem::List(Vec::new()),
                };

                let mut params_items = vec![
                    SvnItem::String(path.as_bytes().to_vec()),
                    rev_tuple,
                    SvnItem::Word(depth.as_word().to_string()),
                    SvnItem::List(
                        fields
                            .iter()
                            .map(|f| SvnItem::Word(f.as_word().to_string()))
                            .collect(),
                    ),
                ];

                if let Some(patterns) = patterns.as_ref()
                    && !patterns.is_empty()
                {
                    params_items.push(SvnItem::List(
                        patterns
                            .iter()
                            .map(|p| SvnItem::String(p.as_bytes().to_vec()))
                            .collect(),
                    ));
                }

                conn.send_command("list", SvnItem::List(params_items))
                    .await?;
                conn.handle_auth_request().await?;

                let mut entries = Vec::new();
                loop {
                    let item = conn.read_item().await?;
                    match item {
                        SvnItem::Word(word) if word == "done" => break,
                        SvnItem::List(items) => entries.push(parse_list_dirent(items)?),
                        other => {
                            return Err(SvnError::Protocol(format!(
                                "unexpected list dirent item: {}",
                                other.kind()
                            )));
                        }
                    }
                }

                let response = conn.read_command_response().await?;
                response.ensure_success("list")?;
                Ok(entries)
            })
        })
        .await
    }

    /// Recursively lists a directory.
    ///
    /// Uses the server `list` capability when available, otherwise falls back
    /// to repeated `get-dir` calls.
    pub async fn list_recursive(
        &mut self,
        path: &str,
        rev: Option<u64>,
    ) -> Result<Vec<DirEntry>, SvnError> {
        let use_list = self
            .conn
            .as_ref()
            .map(|c| c.server_has_cap("list"))
            .unwrap_or(false);

        if use_list {
            return self
                .list(
                    path,
                    rev,
                    Depth::Infinity,
                    &[
                        DirentField::Kind,
                        DirentField::Size,
                        DirentField::HasProps,
                        DirentField::CreatedRev,
                        DirentField::Time,
                        DirentField::LastAuthor,
                    ],
                    None,
                )
                .await;
        }

        let mut out = Vec::new();
        let mut stack = vec![validate_rel_dir_path(path)?];
        while let Some(dir) = stack.pop() {
            let listing = self.list_dir(&dir, rev).await?;
            for entry in &listing.entries {
                if entry.kind == NodeKind::Dir {
                    stack.push(entry.path.clone());
                }
            }
            out.extend(listing.entries);
        }
        Ok(out)
    }
}

fn require_finish_report(report: &Report) -> Result<(), SvnError> {
    match report.commands.last() {
        Some(ReportCommand::FinishReport) => Ok(()),
        _ => Err(SvnError::Protocol(
            "report must end with finish-report".into(),
        )),
    }
}

fn encode_proplist(props: &PropertyList) -> SvnItem {
    SvnItem::List(
        props
            .iter()
            .map(|(name, value)| {
                SvnItem::List(vec![
                    SvnItem::String(name.as_bytes().to_vec()),
                    SvnItem::String(value.clone()),
                ])
            })
            .collect(),
    )
}

fn txn_client_compat_version(ra_client: &str) -> String {
    if let Some(rest) = ra_client.strip_prefix("SVN/") {
        rest.split_whitespace().next().unwrap_or(rest).to_string()
    } else {
        env!("CARGO_PKG_VERSION").to_string()
    }
}

async fn check_for_edit_status(conn: &mut RaSvnConnection) -> Result<(), SvnError> {
    if !conn.data_available().await? {
        return Ok(());
    }

    conn.send_command("abort-edit", SvnItem::List(Vec::new()))
        .await?;
    let response = conn.read_command_response().await?;
    response.ensure_success("abort-edit")?;

    Err(SvnError::Protocol(
        "successful edit status returned too soon".into(),
    ))
}

fn is_unknown_command_error(err: &SvnError) -> bool {
    let message = match err {
        SvnError::Server(server) => server.message_summary(),
        SvnError::Protocol(message) => message.clone(),
        _ => return false,
    };
    let message = message.to_ascii_lowercase();
    message.contains("unknown command") || message.contains("unknown cmd")
}

fn should_drop_connection(err: &SvnError) -> bool {
    matches!(err, SvnError::Protocol(_) | SvnError::Io(_))
}

fn is_retryable_error(err: &SvnError) -> bool {
    match err {
        SvnError::Protocol(msg) => msg.contains("unexpected EOF"),
        SvnError::Io(io) => matches!(
            io.kind(),
            std::io::ErrorKind::ConnectionAborted
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::rasvn::conn::RaSvnConnectionConfig;
    use crate::rasvn::encode_item;
    use std::future::Future;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn run_async<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    async fn connected_session() -> (RaSvnSession, tokio::net::TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accept_task = tokio::spawn(async move { listener.accept().await });
        let client_stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        let (server, _) = accept_task.await.unwrap().unwrap();

        let (read, write) = client_stream.into_split();
        let mut client =
            RaSvnClient::new(SvnUrl::parse("svn://example.com/repo").unwrap(), None, None);
        client.read_timeout = Duration::from_secs(1);
        client.write_timeout = Duration::from_secs(1);
        let conn = RaSvnConnection::new(
            Box::new(read),
            Box::new(write),
            RaSvnConnectionConfig {
                username: None,
                password: None,
                #[cfg(feature = "cyrus-sasl")]
                host: client.base_url.host.clone(),
                #[cfg(feature = "cyrus-sasl")]
                local_addrport: None,
                #[cfg(feature = "cyrus-sasl")]
                remote_addrport: None,
                url: client.base_url.url.clone(),
                ra_client: client.ra_client.clone(),
                read_timeout: client.read_timeout,
                write_timeout: client.write_timeout,
            },
        );

        (
            RaSvnSession {
                client,
                conn: Some(conn),
                server_info: None,
                allow_reconnect: true,
            },
            server,
        )
    }

    fn encode_line(item: &SvnItem) -> Vec<u8> {
        let mut buf = Vec::new();
        encode_item(item, &mut buf);
        buf.push(b'\n');
        buf
    }

    async fn write_item_line(stream: &mut tokio::net::TcpStream, item: &SvnItem) {
        let buf = encode_line(item);
        stream.write_all(&buf).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn read_line(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            let n = stream.read(&mut byte).await.unwrap();
            if n == 0 {
                break;
            }
            buf.push(byte[0]);
            if byte[0] == b'\n' {
                break;
            }
        }
        buf
    }

    fn auth_request(realm: &str) -> SvnItem {
        SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(vec![
                SvnItem::List(Vec::new()),
                SvnItem::String(realm.as_bytes().to_vec()),
            ]),
        ])
    }

    #[test]
    fn open_session_with_stream_runs_handshake_and_disables_reconnect() {
        run_async(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let accept_task = tokio::spawn(async move { listener.accept().await });
            let client_stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            let (mut server, _) = accept_task.await.unwrap().unwrap();

            let server_task = tokio::spawn(async move {
                let greeting = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::Number(2),
                        SvnItem::Number(2),
                        SvnItem::List(Vec::new()),
                        SvnItem::List(vec![
                            SvnItem::Word("edit-pipeline".to_string()),
                            SvnItem::Word("svndiff1".to_string()),
                        ]),
                    ]),
                ]);
                write_item_line(&mut server, &greeting).await;

                let client_greeting = read_line(&mut server).await;
                let expected = SvnItem::List(vec![
                    SvnItem::Number(2),
                    SvnItem::List(vec![
                        SvnItem::Word("edit-pipeline".to_string()),
                        SvnItem::Word("svndiff1".to_string()),
                        SvnItem::Word("accepts-svndiff2".to_string()),
                        SvnItem::Word("absent-entries".to_string()),
                        SvnItem::Word("depth".to_string()),
                        SvnItem::Word("mergeinfo".to_string()),
                        SvnItem::Word("log-revprops".to_string()),
                    ]),
                    SvnItem::String(b"svn://example.com:3690/repo".to_vec()),
                    SvnItem::String(b"test-ra_svn".to_vec()),
                    SvnItem::List(Vec::new()),
                ]);
                assert_eq!(client_greeting, encode_line(&expected));

                write_item_line(&mut server, &auth_request("realm")).await;

                let repos_info = SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(b"uuid".to_vec()),
                        SvnItem::String(b"svn://example.com/repo".to_vec()),
                        SvnItem::List(vec![SvnItem::Word("mergeinfo".to_string())]),
                    ]),
                ]);
                write_item_line(&mut server, &repos_info).await;
            });

            let url = SvnUrl::parse("svn://example.com/repo").unwrap();
            let client = RaSvnClient::new(url, None, None)
                .with_ra_client("test-ra_svn")
                .with_read_timeout(Duration::from_secs(1))
                .with_write_timeout(Duration::from_secs(1));

            let mut session = client
                .open_session_with_stream(client_stream)
                .await
                .unwrap();
            assert!(session.server_info().is_some());

            let err = session.reconnect().await.unwrap_err();
            assert!(matches!(err, SvnError::Protocol(_)));

            server_task.await.unwrap();
        });
    }

    #[test]
    fn update_drives_report_and_editor() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl EditorEventHandler for Collector {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    self.events.push(event);
                    Ok(())
                }
            }

            let report = Report {
                commands: vec![
                    ReportCommand::SetPath {
                        path: "".to_string(),
                        rev: 0,
                        start_empty: true,
                        lock_token: None,
                        depth: Depth::Infinity,
                    },
                    ReportCommand::FinishReport,
                ],
            };

            let expected_update = SvnItem::List(vec![
                SvnItem::Word("update".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(Vec::new()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Bool(true),
                    SvnItem::Word("infinity".to_string()),
                    SvnItem::Bool(false),
                    SvnItem::Bool(false),
                ]),
            ]);
            let expected_set_path = SvnItem::List(vec![
                SvnItem::Word("set-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                    SvnItem::Bool(true),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("infinity".to_string()),
                ]),
            ]);
            let expected_finish_report = SvnItem::List(vec![
                SvnItem::Word("finish-report".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            let expected_cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_update));
                write_item_line(&mut server, &auth_request("realm-1")).await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_set_path)
                );
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_finish_report)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_cmd_success)
                );
                write_item_line(&mut server, &expected_cmd_success).await;
            });

            let mut handler = Collector { events: Vec::new() };
            let options = UpdateOptions::new("", Depth::Infinity).without_copyfrom_args();
            session
                .update(&options, &report, &mut handler)
                .await
                .unwrap();

            server_task.await.unwrap();
            assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
        });
    }

    #[test]
    fn switch_drives_report_and_editor() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl EditorEventHandler for Collector {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    self.events.push(event);
                    Ok(())
                }
            }

            let report = Report {
                commands: vec![
                    ReportCommand::SetPath {
                        path: "".to_string(),
                        rev: 0,
                        start_empty: true,
                        lock_token: None,
                        depth: Depth::Infinity,
                    },
                    ReportCommand::FinishReport,
                ],
            };

            let switch_url = SvnUrl::parse("svn://example.com/repo/branch").unwrap();
            let switch_url = switch_url.url;

            let expected_switch = SvnItem::List(vec![
                SvnItem::Word("switch".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(Vec::new()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Bool(true),
                    SvnItem::String(switch_url.as_bytes().to_vec()),
                    SvnItem::Word("infinity".to_string()),
                    SvnItem::Bool(false),
                    SvnItem::Bool(false),
                ]),
            ]);

            let expected_set_path = SvnItem::List(vec![
                SvnItem::Word("set-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                    SvnItem::Bool(true),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("infinity".to_string()),
                ]),
            ]);
            let expected_finish_report = SvnItem::List(vec![
                SvnItem::Word("finish-report".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            let expected_cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_switch));
                write_item_line(&mut server, &auth_request("realm-1")).await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_set_path)
                );
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_finish_report)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_cmd_success)
                );
                write_item_line(&mut server, &expected_cmd_success).await;
            });

            let mut handler = Collector { events: Vec::new() };
            let options =
                SwitchOptions::new("", switch_url, Depth::Infinity).without_copyfrom_args();
            session
                .switch(&options, &report, &mut handler)
                .await
                .unwrap();

            server_task.await.unwrap();
            assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
        });
    }

    #[test]
    fn status_drives_report_and_editor() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl EditorEventHandler for Collector {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    self.events.push(event);
                    Ok(())
                }
            }

            let report = Report {
                commands: vec![
                    ReportCommand::SetPath {
                        path: "".to_string(),
                        rev: 0,
                        start_empty: true,
                        lock_token: None,
                        depth: Depth::Infinity,
                    },
                    ReportCommand::FinishReport,
                ],
            };

            let expected_status = SvnItem::List(vec![
                SvnItem::Word("status".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(Vec::new()),
                    SvnItem::Bool(true),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("infinity".to_string()),
                ]),
            ]);

            let expected_set_path = SvnItem::List(vec![
                SvnItem::Word("set-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                    SvnItem::Bool(true),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("infinity".to_string()),
                ]),
            ]);
            let expected_finish_report = SvnItem::List(vec![
                SvnItem::Word("finish-report".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            let expected_cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_status));
                write_item_line(&mut server, &auth_request("realm-1")).await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_set_path)
                );
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_finish_report)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_cmd_success)
                );
                write_item_line(&mut server, &expected_cmd_success).await;
            });

            let mut handler = Collector { events: Vec::new() };
            let options = StatusOptions::new("", Depth::Infinity);
            session
                .status(&options, &report, &mut handler)
                .await
                .unwrap();

            server_task.await.unwrap();
            assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
        });
    }

    #[test]
    fn diff_drives_report_and_editor() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl EditorEventHandler for Collector {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    self.events.push(event);
                    Ok(())
                }
            }

            let report = Report {
                commands: vec![
                    ReportCommand::SetPath {
                        path: "".to_string(),
                        rev: 0,
                        start_empty: true,
                        lock_token: None,
                        depth: Depth::Infinity,
                    },
                    ReportCommand::FinishReport,
                ],
            };

            let versus_url = SvnUrl::parse("svn://example.com/repo/branch").unwrap().url;

            let expected_diff = SvnItem::List(vec![
                SvnItem::Word("diff".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(Vec::new()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Bool(true),
                    SvnItem::Bool(false),
                    SvnItem::String(versus_url.as_bytes().to_vec()),
                    SvnItem::Bool(true),
                    SvnItem::Word("infinity".to_string()),
                ]),
            ]);

            let expected_set_path = SvnItem::List(vec![
                SvnItem::Word("set-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                    SvnItem::Bool(true),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("infinity".to_string()),
                ]),
            ]);
            let expected_finish_report = SvnItem::List(vec![
                SvnItem::Word("finish-report".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            let expected_cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_diff));
                write_item_line(&mut server, &auth_request("realm-1")).await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_set_path)
                );
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_finish_report)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_cmd_success)
                );
                write_item_line(&mut server, &expected_cmd_success).await;
            });

            let mut handler = Collector { events: Vec::new() };
            let options = DiffOptions::new("", versus_url, Depth::Infinity);
            session.diff(&options, &report, &mut handler).await.unwrap();

            server_task.await.unwrap();
            assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
        });
    }

    #[test]
    fn replay_range_emits_revprops_and_finish_replay() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl EditorEventHandler for Collector {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    self.events.push(event);
                    Ok(())
                }
            }

            let expected_replay_range = SvnItem::List(vec![
                SvnItem::Word("replay-range".to_string()),
                SvnItem::List(vec![
                    SvnItem::Number(1),
                    SvnItem::Number(2),
                    SvnItem::Number(0),
                    SvnItem::Bool(true),
                ]),
            ]);

            let revprops_1 = SvnItem::List(vec![
                SvnItem::Word("revprops".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::String(b"svn:author".to_vec()),
                    SvnItem::String(b"alice".to_vec()),
                ])]),
            ]);
            let revprops_2 = SvnItem::List(vec![
                SvnItem::Word("revprops".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::String(b"svn:author".to_vec()),
                    SvnItem::String(b"bob".to_vec()),
                ])]),
            ]);
            let finish_replay = SvnItem::List(vec![
                SvnItem::Word("finish-replay".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_replay_range)
                );
                write_item_line(&mut server, &auth_request("realm")).await;

                write_item_line(&mut server, &revprops_1).await;
                write_item_line(&mut server, &finish_replay).await;
                write_item_line(&mut server, &revprops_2).await;
                write_item_line(&mut server, &finish_replay).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let mut handler = Collector { events: Vec::new() };
            let options = ReplayRangeOptions::new(1, 2);
            session.replay_range(&options, &mut handler).await.unwrap();

            server_task.await.unwrap();
            assert_eq!(handler.events.len(), 4);
            assert!(matches!(handler.events[0], EditorEvent::RevProps { .. }));
            assert_eq!(handler.events[1], EditorEvent::FinishReplay);
            assert!(matches!(handler.events[2], EditorEvent::RevProps { .. }));
            assert_eq!(handler.events[3], EditorEvent::FinishReplay);

            let props = match &handler.events[0] {
                EditorEvent::RevProps { props } => Some(props),
                _ => None,
            }
            .unwrap();
            assert_eq!(
                props.get("svn:author").map(|v| v.as_slice()),
                Some(b"alice".as_slice())
            );

            let props = match &handler.events[2] {
                EditorEvent::RevProps { props } => Some(props),
                _ => None,
            }
            .unwrap();
            assert_eq!(
                props.get("svn:author").map(|v| v.as_slice()),
                Some(b"bob".as_slice())
            );
        });
    }

    #[test]
    fn get_file_with_iprops_uses_get_iprops_command() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;
            session.server_info = Some(ServerInfo {
                server_caps: vec!["inherited-props".to_string()],
                repository: crate::RepositoryInfo {
                    uuid: "uuid".to_string(),
                    root_url: "svn://example.com/repo".to_string(),
                    capabilities: Vec::new(),
                },
            });

            let expected_get_file = SvnItem::List(vec![
                SvnItem::Word("get-file".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                    SvnItem::Bool(false), // want-props
                    SvnItem::Bool(true),  // want-contents
                    SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
                ]),
            ]);

            let expected_get_iprops = SvnItem::List(vec![
                SvnItem::Word("get-iprops".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_file)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![
                            SvnItem::List(Vec::new()),
                            SvnItem::Number(5),
                            SvnItem::List(Vec::new()),
                        ]),
                    ]),
                )
                .await;
                write_item_line(&mut server, &SvnItem::String(b"data".to_vec())).await;
                write_item_line(&mut server, &SvnItem::String(Vec::new())).await;
                write_item_line(&mut server, &cmd_success).await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_iprops)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::List(vec![SvnItem::List(vec![
                            SvnItem::String(b"/".to_vec()),
                            SvnItem::List(vec![SvnItem::List(vec![
                                SvnItem::String(b"p".to_vec()),
                                SvnItem::String(b"v".to_vec()),
                            ])]),
                        ])])]),
                    ]),
                )
                .await;
            });

            let mut out = tokio::io::sink();
            let options = GetFileOptions {
                rev: 5,
                want_props: false,
                want_iprops: true,
                max_bytes: 1024,
            };
            let result = session
                .get_file_with_options("trunk/file.txt", &options, &mut out)
                .await
                .unwrap();

            assert_eq!(result.bytes_written, 4);
            assert_eq!(result.inherited_props.len(), 1);
            assert_eq!(result.inherited_props[0].path, "/");
            assert_eq!(result.inherited_props[0].props.get("p").unwrap(), b"v");

            server_task.await.unwrap();
        });
    }

    #[test]
    fn get_latest_rev_sends_command_and_parses_response() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected = SvnItem::List(vec![
                SvnItem::Word("get-latest-rev".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Number(42)]),
                    ]),
                )
                .await;
            });

            let rev = session.get_latest_rev().await.unwrap();
            assert_eq!(rev, 42);

            server_task.await.unwrap();
        });
    }

    #[test]
    fn get_dated_rev_sends_command_and_parses_response() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected = SvnItem::List(vec![
                SvnItem::Word("get-dated-rev".to_string()),
                SvnItem::List(vec![SvnItem::String(b"2025-01-01T00:00:00Z".to_vec())]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Number(7)]),
                    ]),
                )
                .await;
            });

            let rev = session.get_dated_rev("2025-01-01T00:00:00Z").await.unwrap();
            assert_eq!(rev, 7);

            server_task.await.unwrap();
        });
    }

    #[test]
    fn rev_proplist_and_rev_prop_round_trip() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_rev_proplist = SvnItem::List(vec![
                SvnItem::Word("rev-proplist".to_string()),
                SvnItem::List(vec![SvnItem::Number(5)]),
            ]);
            let expected_rev_prop = SvnItem::List(vec![
                SvnItem::Word("rev-prop".to_string()),
                SvnItem::List(vec![
                    SvnItem::Number(5),
                    SvnItem::String(b"svn:log".to_vec()),
                ]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_rev_proplist)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::List(vec![SvnItem::List(vec![
                            SvnItem::String(b"p".to_vec()),
                            SvnItem::String(b"v".to_vec()),
                        ])])]),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_rev_prop)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::List(vec![SvnItem::String(
                            b"hello".to_vec(),
                        )])]),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_rev_prop)
                );
                write_item_line(&mut server, &auth_request("realm-3")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::List(Vec::new())]),
                    ]),
                )
                .await;
            });

            let props = session.rev_proplist(5).await.unwrap();
            assert_eq!(props.get("p").unwrap().as_slice(), b"v");

            let value = session.rev_prop(5, "svn:log").await.unwrap();
            assert_eq!(value.as_deref(), Some(b"hello".as_slice()));

            let value = session.rev_prop(5, "svn:log").await.unwrap();
            assert_eq!(value, None);

            server_task.await.unwrap();
        });
    }

    #[test]
    fn check_path_sends_command_and_parses_kind() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_check_path = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(2)]),
                ]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_check_path)
                );
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word("file".to_string())]),
                    ]),
                )
                .await;
            });

            let kind = session.check_path("trunk/file.txt", Some(2)).await.unwrap();
            assert_eq!(kind, NodeKind::File);

            server_task.await.unwrap();
        });
    }

    #[test]
    fn list_dir_sends_expected_get_dir_params_and_parses_entries() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_get_dir = SvnItem::List(vec![
                SvnItem::Word("get-dir".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(false), // want-props
                    SvnItem::Bool(true),  // want-contents
                    SvnItem::List(vec![
                        SvnItem::Word("kind".to_string()),
                        SvnItem::Word("size".to_string()),
                        SvnItem::Word("has-props".to_string()),
                        SvnItem::Word("created-rev".to_string()),
                        SvnItem::Word("time".to_string()),
                        SvnItem::Word("last-author".to_string()),
                    ]),
                    SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
                ]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_get_dir));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![
                            SvnItem::Number(9),
                            SvnItem::List(Vec::new()),
                            SvnItem::List(vec![SvnItem::List(vec![
                                SvnItem::String(b"file.txt".to_vec()),
                                SvnItem::Word("file".to_string()),
                                SvnItem::Number(3),
                                SvnItem::Bool(false),
                                SvnItem::Number(9),
                            ])]),
                        ]),
                    ]),
                )
                .await;
            });

            let listing = session.list_dir("trunk", None).await.unwrap();
            assert_eq!(listing.rev, 9);
            assert_eq!(listing.entries.len(), 1);
            assert_eq!(listing.entries[0].name, "file.txt");
            assert_eq!(listing.entries[0].path, "trunk/file.txt");
            assert_eq!(listing.entries[0].kind, NodeKind::File);
            assert_eq!(listing.entries[0].size, Some(3));

            server_task.await.unwrap();
        });
    }

    #[test]
    fn list_sends_patterns_and_reads_dirents_until_done() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_list = SvnItem::List(vec![
                SvnItem::Word("list".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("infinity".to_string()),
                    SvnItem::List(vec![SvnItem::Word("kind".to_string())]),
                    SvnItem::List(vec![SvnItem::String(b"*.rs".to_vec())]),
                ]),
            ]);

            let dirent = SvnItem::List(vec![
                SvnItem::String(b"trunk/main.rs".to_vec()),
                SvnItem::Word("file".to_string()),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_list));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(&mut server, &dirent).await;
                write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let patterns = vec![String::from("*.rs")];
            let fields = [DirentField::Kind];
            let entries = session
                .list(
                    "trunk",
                    None,
                    Depth::Infinity,
                    &fields,
                    Some(patterns.as_slice()),
                )
                .await
                .unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].path, "trunk/main.rs");
            assert_eq!(entries[0].name, "main.rs");
            assert_eq!(entries[0].kind, NodeKind::File);

            server_task.await.unwrap();
        });
    }

    #[test]
    fn list_recursive_falls_back_to_get_dir_when_list_cap_missing() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_get_dir_trunk = SvnItem::List(vec![
                SvnItem::Word("get-dir".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(false), // want-props
                    SvnItem::Bool(true),  // want-contents
                    SvnItem::List(vec![
                        SvnItem::Word("kind".to_string()),
                        SvnItem::Word("size".to_string()),
                        SvnItem::Word("has-props".to_string()),
                        SvnItem::Word("created-rev".to_string()),
                        SvnItem::Word("time".to_string()),
                        SvnItem::Word("last-author".to_string()),
                    ]),
                    SvnItem::Bool(false), // want-iprops
                ]),
            ]);

            let expected_get_dir_sub = SvnItem::List(vec![
                SvnItem::Word("get-dir".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/sub".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(false), // want-props
                    SvnItem::Bool(true),  // want-contents
                    SvnItem::List(vec![
                        SvnItem::Word("kind".to_string()),
                        SvnItem::Word("size".to_string()),
                        SvnItem::Word("has-props".to_string()),
                        SvnItem::Word("created-rev".to_string()),
                        SvnItem::Word("time".to_string()),
                        SvnItem::Word("last-author".to_string()),
                    ]),
                    SvnItem::Bool(false), // want-iprops
                ]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_dir_trunk)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![
                            SvnItem::Number(9),
                            SvnItem::List(Vec::new()),
                            SvnItem::List(vec![
                                SvnItem::List(vec![
                                    SvnItem::String(b"sub".to_vec()),
                                    SvnItem::Word("dir".to_string()),
                                    SvnItem::Number(0),
                                    SvnItem::Bool(false),
                                    SvnItem::Number(9),
                                ]),
                                SvnItem::List(vec![
                                    SvnItem::String(b"a.txt".to_vec()),
                                    SvnItem::Word("file".to_string()),
                                    SvnItem::Number(1),
                                    SvnItem::Bool(false),
                                    SvnItem::Number(9),
                                ]),
                            ]),
                        ]),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_dir_sub)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![
                            SvnItem::Number(9),
                            SvnItem::List(Vec::new()),
                            SvnItem::List(vec![SvnItem::List(vec![
                                SvnItem::String(b"b.txt".to_vec()),
                                SvnItem::Word("file".to_string()),
                                SvnItem::Number(2),
                                SvnItem::Bool(false),
                                SvnItem::Number(9),
                            ])]),
                        ]),
                    ]),
                )
                .await;
            });

            let mut entries = session.list_recursive("trunk", None).await.unwrap();
            entries.sort_by(|a, b| a.path.cmp(&b.path));
            assert_eq!(entries.len(), 3);
            assert_eq!(entries[0].path, "trunk/a.txt");
            assert_eq!(entries[1].path, "trunk/sub");
            assert_eq!(entries[2].path, "trunk/sub/b.txt");

            server_task.await.unwrap();
        });
    }

    #[test]
    fn list_recursive_uses_list_capability_when_available() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;
            session
                .conn
                .as_mut()
                .unwrap()
                .set_server_caps_for_test(&["list"]);

            let expected_list = SvnItem::List(vec![
                SvnItem::Word("list".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("infinity".to_string()),
                    SvnItem::List(vec![
                        SvnItem::Word("kind".to_string()),
                        SvnItem::Word("size".to_string()),
                        SvnItem::Word("has-props".to_string()),
                        SvnItem::Word("created-rev".to_string()),
                        SvnItem::Word("time".to_string()),
                        SvnItem::Word("last-author".to_string()),
                    ]),
                ]),
            ]);

            let dirent = SvnItem::List(vec![
                SvnItem::String(b"trunk/main.rs".to_vec()),
                SvnItem::Word("file".to_string()),
                SvnItem::Number(3),
                SvnItem::Bool(false),
                SvnItem::Number(9),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_list));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(&mut server, &dirent).await;
                write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let entries = session.list_recursive("trunk", None).await.unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].path, "trunk/main.rs");
            assert_eq!(entries[0].kind, NodeKind::File);

            server_task.await.unwrap();
        });
    }

    #[test]
    fn stat_returns_none_when_check_path_reports_none() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_stat = SvnItem::List(vec![
                SvnItem::Word("stat".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/missing.txt".to_vec()),
                    SvnItem::List(Vec::new()),
                ]),
            ]);

            let expected_check_path = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/missing.txt".to_vec()),
                    SvnItem::List(Vec::new()),
                ]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_stat));
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_check_path)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word("none".to_string())]),
                    ]),
                )
                .await;
            });

            let stat = session.stat("trunk/missing.txt", None).await.unwrap();
            assert_eq!(stat, None);

            server_task.await.unwrap();
        });
    }

    #[test]
    fn get_mergeinfo_sends_command_and_parses_catalog() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_mergeinfo = SvnItem::List(vec![
                SvnItem::Word("get-mergeinfo".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(vec![SvnItem::String(b"trunk".to_vec())]),
                    SvnItem::List(Vec::new()),
                    SvnItem::Word("explicit".to_string()),
                    SvnItem::Bool(false),
                ]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_mergeinfo)
                );
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::List(vec![SvnItem::List(vec![
                            SvnItem::String(b"/trunk".to_vec()),
                            SvnItem::String(b"/trunk:1-2".to_vec()),
                        ])])]),
                    ]),
                )
                .await;
            });

            let paths = vec![String::from("trunk")];
            let out = session
                .get_mergeinfo(&paths, None, MergeInfoInheritance::Explicit, false)
                .await
                .unwrap();
            assert_eq!(out.get("trunk").map(String::as_str), Some("/trunk:1-2"));

            server_task.await.unwrap();
        });
    }

    #[test]
    fn get_locations_reads_entries_until_done() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_get_locations = SvnItem::List(vec![
                SvnItem::Word("get-locations".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::Number(9),
                    SvnItem::List(vec![SvnItem::Number(1), SvnItem::Number(2)]),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_locations)
                );
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Number(1),
                        SvnItem::String(b"/trunk/file.txt".to_vec()),
                    ]),
                )
                .await;
                write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let revs = [1u64, 2u64];
            let entries = session
                .get_locations("trunk/file.txt", 9, &revs)
                .await
                .unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].rev, 1);
            assert_eq!(entries[0].path, "trunk/file.txt");

            server_task.await.unwrap();
        });
    }

    #[test]
    fn get_location_segments_reads_entries_until_done() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected = SvnItem::List(vec![
                SvnItem::Word("get-location-segments".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(9)]),
                    SvnItem::List(Vec::new()),
                    SvnItem::List(Vec::new()),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Number(1),
                        SvnItem::Number(2),
                        SvnItem::String(b"/trunk/file.txt".to_vec()),
                    ]),
                )
                .await;
                write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let segments = session
                .get_location_segments("trunk/file.txt", 9, None, None)
                .await
                .unwrap();
            assert_eq!(segments.len(), 1);
            assert_eq!(segments[0].range_start, 1);
            assert_eq!(segments[0].range_end, 2);
            assert_eq!(segments[0].path.as_deref(), Some("/trunk/file.txt"));

            server_task.await.unwrap();
        });
    }

    #[test]
    fn get_file_revs_reads_entries_and_delta_chunks() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_get_file_revs = SvnItem::List(vec![
                SvnItem::Word("get-file-revs".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(1)]),
                    SvnItem::List(vec![SvnItem::Number(2)]),
                    SvnItem::Bool(false),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_file_revs)
                );
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::String(b"/trunk/file.txt".to_vec()),
                        SvnItem::Number(2),
                        SvnItem::List(vec![SvnItem::List(vec![
                            SvnItem::String(b"svn:author".to_vec()),
                            SvnItem::String(b"alice".to_vec()),
                        ])]),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;
                write_item_line(&mut server, &SvnItem::String(b"delta".to_vec())).await;
                write_item_line(&mut server, &SvnItem::String(Vec::new())).await;
                write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let revs = session
                .get_file_revs("trunk/file.txt", Some(1), Some(2), false)
                .await
                .unwrap();
            assert_eq!(revs.len(), 1);
            assert_eq!(revs[0].path, "trunk/file.txt");
            assert_eq!(revs[0].rev, 2);
            assert_eq!(revs[0].rev_props.get("svn:author").unwrap(), b"alice");
            assert_eq!(revs[0].delta_chunks, vec![b"delta".to_vec()]);

            server_task.await.unwrap();
        });
    }

    #[test]
    fn get_deleted_rev_returns_none_on_missing_revision_failure() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected = SvnItem::List(vec![
                SvnItem::Word("get-deleted-rev".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::Number(5),
                    SvnItem::Number(7),
                ]),
            ]);

            let cmd_failure = SvnItem::List(vec![
                SvnItem::Word("failure".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::Number(123),
                    SvnItem::String(b"missing revision".to_vec()),
                    SvnItem::String(Vec::new()),
                    SvnItem::Number(0),
                ])]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(vec![SvnItem::Number(9)]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(&mut server, &cmd_failure).await;

                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let deleted = session
                .get_deleted_rev("trunk/file.txt", 5, 7)
                .await
                .unwrap();
            assert_eq!(deleted, None);

            let deleted = session
                .get_deleted_rev("trunk/file.txt", 5, 7)
                .await
                .unwrap();
            assert_eq!(deleted, Some(9));

            server_task.await.unwrap();
        });
    }

    #[test]
    fn get_lock_and_get_locks_parse_lockdesc() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_get_lock = SvnItem::List(vec![
                SvnItem::Word("get-lock".to_string()),
                SvnItem::List(vec![SvnItem::String(b"trunk/file.txt".to_vec())]),
            ]);

            let expected_get_locks = SvnItem::List(vec![
                SvnItem::Word("get-locks".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk".to_vec()),
                    SvnItem::List(vec![SvnItem::Word("infinity".to_string())]),
                ]),
            ]);

            let lockdesc = SvnItem::List(vec![
                SvnItem::String(b"/trunk/file.txt".to_vec()),
                SvnItem::String(b"token".to_vec()),
                SvnItem::String(b"alice".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::String(b"2025-01-01".to_vec()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_lock)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::List(vec![lockdesc.clone()])]),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_locks)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::List(vec![lockdesc])]),
                    ]),
                )
                .await;
            });

            let lock = session.get_lock("trunk/file.txt").await.unwrap().unwrap();
            assert_eq!(lock.path, "trunk/file.txt");
            assert_eq!(lock.owner, "alice");
            assert_eq!(lock.token, "token");

            let locks = session.get_locks("trunk", Depth::Infinity).await.unwrap();
            assert_eq!(locks.len(), 1);
            assert_eq!(locks[0].path, "trunk/file.txt");

            server_task.await.unwrap();
        });
    }

    #[test]
    fn replay_sends_command_and_drives_editor() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            struct Collector {
                events: Vec<EditorEvent>,
            }

            impl EditorEventHandler for Collector {
                fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                    self.events.push(event);
                    Ok(())
                }
            }

            let expected_replay = SvnItem::List(vec![
                SvnItem::Word("replay".to_string()),
                SvnItem::List(vec![
                    SvnItem::Number(5),
                    SvnItem::Number(0),
                    SvnItem::Bool(true),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_replay));
                write_item_line(&mut server, &auth_request("realm")).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("close-edit".to_string()),
                        SvnItem::List(Vec::new()),
                    ]),
                )
                .await;

                assert_eq!(read_line(&mut server).await, encode_line(&cmd_success));
                write_item_line(&mut server, &cmd_success).await;
            });

            let mut handler = Collector { events: Vec::new() };
            let options = ReplayOptions::new(5);
            session.replay(&options, &mut handler).await.unwrap();

            server_task.await.unwrap();
            assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
        });
    }

    #[test]
    fn reparent_sends_command_and_updates_base_url() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let new_url = SvnUrl::parse("svn://example.com/repo/branch").unwrap();
            let expected_reparent = SvnItem::List(vec![
                SvnItem::Word("reparent".to_string()),
                SvnItem::List(vec![SvnItem::String(new_url.url.as_bytes().to_vec())]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_reparent)
                );
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            session.reparent(new_url.clone()).await.unwrap();
            assert_eq!(session.client.base_url, new_url);

            server_task.await.unwrap();
        });
    }

    #[test]
    fn proplist_file_uses_get_file_without_extra_params() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_check_path = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                ]),
            ]);

            let expected_get_file = SvnItem::List(vec![
                SvnItem::Word("get-file".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                    SvnItem::Bool(true),  // want-props
                    SvnItem::Bool(false), // want-contents
                    SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
                ]),
            ]);

            let response_file_props = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(Vec::new()),
                    SvnItem::Number(5),
                    SvnItem::List(vec![SvnItem::List(vec![
                        SvnItem::String(b"p".to_vec()),
                        SvnItem::String(b"v".to_vec()),
                    ])]),
                ]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_check_path)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word("file".to_string())]),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_file)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(&mut server, &response_file_props).await;
            });

            let props = session
                .proplist("trunk/file.txt", Some(5))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(props.get("p").unwrap().as_slice(), b"v");

            server_task.await.unwrap();
        });
    }

    #[test]
    fn proplist_dir_sends_dirent_fields_placeholder() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_check_path = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                ]),
            ]);

            let expected_get_dir = SvnItem::List(vec![
                SvnItem::Word("get-dir".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(5)]),
                    SvnItem::Bool(true),  // want-props
                    SvnItem::Bool(false), // want-contents
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
                ]),
            ]);

            let response_dir_props = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(vec![
                    SvnItem::Number(5),
                    SvnItem::List(vec![SvnItem::List(vec![
                        SvnItem::String(b"p".to_vec()),
                        SvnItem::String(b"v".to_vec()),
                    ])]),
                    SvnItem::List(Vec::new()),
                ]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_check_path)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word("dir".to_string())]),
                    ]),
                )
                .await;

                assert_eq!(read_line(&mut server).await, encode_line(&expected_get_dir));
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(&mut server, &response_dir_props).await;
            });

            let props = session.proplist("trunk", Some(5)).await.unwrap().unwrap();
            assert_eq!(props.get("p").unwrap().as_slice(), b"v");

            server_task.await.unwrap();
        });
    }

    #[test]
    fn get_file_revs_errors_on_empty_result() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_get_file_revs = SvnItem::List(vec![
                SvnItem::Word("get-file-revs".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/file.txt".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(false),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_get_file_revs)
                );
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let err = session
                .get_file_revs("trunk/file.txt", None, None, false)
                .await
                .unwrap_err();
            assert!(
                matches!(err, SvnError::Protocol(msg) if msg == "The get-file-revs command didn't return any revisions")
            );

            server_task.await.unwrap();
        });
    }

    #[test]
    fn log_merges_requested_revprops_into_map() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_log = SvnItem::List(vec![
                SvnItem::Word("log".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(vec![SvnItem::String(b"trunk".to_vec())]),
                    SvnItem::List(vec![SvnItem::Number(1)]),
                    SvnItem::List(vec![SvnItem::Number(2)]),
                    SvnItem::Bool(false),
                    SvnItem::Bool(true),
                    SvnItem::Number(0),
                    SvnItem::Bool(false),
                    SvnItem::Word("revprops".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(b"svn:author".to_vec()),
                        SvnItem::String(b"svn:custom".to_vec()),
                    ]),
                ]),
            ]);

            let log_entry_item = SvnItem::List(vec![
                SvnItem::List(Vec::new()),
                SvnItem::Number(10),
                SvnItem::List(vec![SvnItem::String(b"alice".to_vec())]),
                SvnItem::List(vec![SvnItem::String(b"2025-01-01".to_vec())]),
                SvnItem::List(vec![SvnItem::String(b"msg".to_vec())]),
                SvnItem::Bool(false),
                SvnItem::Bool(false),
                SvnItem::Number(1),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::String(b"svn:custom".to_vec()),
                    SvnItem::String(b"x".to_vec()),
                ])]),
                SvnItem::Bool(false),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_log));
                write_item_line(&mut server, &auth_request("realm-1")).await;

                write_item_line(&mut server, &log_entry_item).await;
                write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let options = LogOptions {
                target_paths: vec!["trunk".to_string()],
                start_rev: Some(1),
                end_rev: Some(2),
                changed_paths: false,
                strict_node: true,
                limit: 0,
                include_merged_revisions: false,
                revprops: LogRevProps::Custom(vec![
                    "svn:author".to_string(),
                    "svn:custom".to_string(),
                ]),
            };

            let entries = session.log_with_options(&options).await.unwrap();
            assert_eq!(entries.len(), 1);
            let entry = &entries[0];
            assert_eq!(entry.rev, 10);
            assert_eq!(entry.author.as_deref(), Some("alice"));
            assert_eq!(entry.date.as_deref(), Some("2025-01-01"));
            assert_eq!(entry.message.as_deref(), Some("msg"));
            assert_eq!(entry.rev_props.get("svn:custom").unwrap(), b"x");
            assert_eq!(entry.rev_props.get("svn:author").unwrap(), b"alice");
            assert!(!entry.rev_props.contains_key("svn:date"));
            assert!(!entry.rev_props.contains_key("svn:log"));

            server_task.await.unwrap();
        });
    }

    #[test]
    fn change_rev_prop_encodes_optional_value() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_with_value = SvnItem::List(vec![
                SvnItem::Word("change-rev-prop".to_string()),
                SvnItem::List(vec![
                    SvnItem::Number(9),
                    SvnItem::String(b"svn:log".to_vec()),
                    SvnItem::String(b"msg".to_vec()),
                ]),
            ]);

            let expected_without_value = SvnItem::List(vec![
                SvnItem::Word("change-rev-prop".to_string()),
                SvnItem::List(vec![
                    SvnItem::Number(9),
                    SvnItem::String(b"svn:log".to_vec()),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_with_value)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(&mut server, &cmd_success).await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_without_value)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            session
                .change_rev_prop(9, "svn:log", Some(b"msg".to_vec()))
                .await
                .unwrap();
            session.change_rev_prop(9, "svn:log", None).await.unwrap();

            server_task.await.unwrap();
        });
    }

    #[test]
    fn change_rev_prop2_encodes_value_tuple_and_conditional() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected = SvnItem::List(vec![
                SvnItem::Word("change-rev-prop2".to_string()),
                SvnItem::List(vec![
                    SvnItem::Number(9),
                    SvnItem::String(b"svn:log".to_vec()),
                    SvnItem::List(vec![SvnItem::String(b"new".to_vec())]),
                    SvnItem::List(vec![SvnItem::Bool(false), SvnItem::String(b"old".to_vec())]),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            session
                .change_rev_prop2(
                    9,
                    "svn:log",
                    Some(b"new".to_vec()),
                    false,
                    Some(b"old".to_vec()),
                )
                .await
                .unwrap();

            server_task.await.unwrap();
        });
    }

    #[test]
    fn lock_does_not_drop_connection_on_server_failure() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_lock = SvnItem::List(vec![
                SvnItem::Word("lock".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/a.txt".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(false),
                    SvnItem::List(Vec::new()),
                ]),
            ]);

            let cmd_failure = SvnItem::List(vec![
                SvnItem::Word("failure".to_string()),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::Number(123),
                    SvnItem::String(b"lock denied".to_vec()),
                    SvnItem::String(b"file".to_vec()),
                    SvnItem::Number(1),
                ])]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_lock));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(&mut server, &cmd_failure).await;
            });

            let err = session
                .lock("trunk/a.txt", &LockOptions::new())
                .await
                .unwrap_err();
            assert!(matches!(err, SvnError::Server(_)));
            assert!(session.conn.is_some());

            server_task.await.unwrap();
        });
    }

    #[test]
    fn lock_and_unlock_round_trip() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_lock = SvnItem::List(vec![
                SvnItem::Word("lock".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/a.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
                    SvnItem::Bool(false),
                    SvnItem::List(vec![SvnItem::Number(12)]),
                ]),
            ]);

            let lockdesc = SvnItem::List(vec![
                SvnItem::String(b"/trunk/a.txt".to_vec()),
                SvnItem::String(b"t0".to_vec()),
                SvnItem::String(b"alice".to_vec()),
                SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
                SvnItem::String(b"2025-01-01".to_vec()),
                SvnItem::List(Vec::new()),
            ]);

            let expected_unlock = SvnItem::List(vec![
                SvnItem::Word("unlock".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/a.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::String(b"t0".to_vec())]),
                    SvnItem::Bool(false),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_lock));
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![lockdesc]),
                    ]),
                )
                .await;

                assert_eq!(read_line(&mut server).await, encode_line(&expected_unlock));
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let lock = session
                .lock(
                    "trunk/a.txt",
                    &LockOptions::new().with_comment("hi").with_current_rev(12),
                )
                .await
                .unwrap();
            assert_eq!(lock.path, "trunk/a.txt");
            assert_eq!(lock.token, "t0");
            assert_eq!(lock.owner, "alice");
            assert_eq!(lock.comment.as_deref(), Some("hi"));

            session
                .unlock("trunk/a.txt", &UnlockOptions::new().with_token("t0"))
                .await
                .unwrap();

            server_task.await.unwrap();
        });
    }

    #[test]
    fn lock_many_and_unlock_many_stream_results() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_lock_many = SvnItem::List(vec![
                SvnItem::Word("lock-many".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(true),
                    SvnItem::List(vec![
                        SvnItem::List(vec![
                            SvnItem::String(b"trunk/a.txt".to_vec()),
                            SvnItem::List(vec![SvnItem::Number(1)]),
                        ]),
                        SvnItem::List(vec![
                            SvnItem::String(b"trunk/b.txt".to_vec()),
                            SvnItem::List(Vec::new()),
                        ]),
                    ]),
                ]),
            ]);

            let lock_a = SvnItem::List(vec![
                SvnItem::String(b"trunk/a.txt".to_vec()),
                SvnItem::String(b"t1".to_vec()),
                SvnItem::String(b"alice".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::String(b"2025-01-01".to_vec()),
            ]);

            let err = SvnItem::List(vec![
                SvnItem::Number(123),
                SvnItem::String(b"lock denied".to_vec()),
                SvnItem::String(b"file".to_vec()),
                SvnItem::Number(1),
            ]);

            let expected_unlock_many = SvnItem::List(vec![
                SvnItem::Word("unlock-many".to_string()),
                SvnItem::List(vec![
                    SvnItem::Bool(false),
                    SvnItem::List(vec![
                        SvnItem::List(vec![
                            SvnItem::String(b"trunk/a.txt".to_vec()),
                            SvnItem::List(vec![SvnItem::String(b"t1".to_vec())]),
                        ]),
                        SvnItem::List(vec![
                            SvnItem::String(b"trunk/b.txt".to_vec()),
                            SvnItem::List(Vec::new()),
                        ]),
                    ]),
                ]),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_lock_many)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;

                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![lock_a]),
                    ]),
                )
                .await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("failure".to_string()),
                        SvnItem::List(vec![err.clone()]),
                    ]),
                )
                .await;
                write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                write_item_line(&mut server, &cmd_success).await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_unlock_many)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::String(b"/trunk/a.txt".to_vec())]),
                    ]),
                )
                .await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("failure".to_string()),
                        SvnItem::List(vec![err]),
                    ]),
                )
                .await;
                write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let lock_results = session
                .lock_many(
                    &LockManyOptions::new().steal_lock(),
                    &[
                        LockTarget::new("trunk/a.txt").with_current_rev(1),
                        LockTarget::new("trunk/b.txt"),
                    ],
                )
                .await
                .unwrap();
            assert_eq!(lock_results.len(), 2);
            assert_eq!(lock_results[0].as_ref().unwrap().token, "t1");
            assert!(matches!(lock_results[1], Err(SvnError::Server(_))));

            let unlock_results = session
                .unlock_many(
                    &UnlockManyOptions::new(),
                    &[
                        UnlockTarget::new("trunk/a.txt").with_token("t1"),
                        UnlockTarget::new("trunk/b.txt"),
                    ],
                )
                .await
                .unwrap();
            assert_eq!(unlock_results.len(), 2);
            assert_eq!(unlock_results[0].as_ref().unwrap(), "trunk/a.txt");
            assert!(matches!(unlock_results[1], Err(SvnError::Server(_))));

            server_task.await.unwrap();
        });
    }

    #[test]
    fn lock_many_falls_back_to_lock_when_unsupported() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_lock_many = SvnItem::List(vec![
                SvnItem::Word("lock-many".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
                    SvnItem::Bool(true),
                    SvnItem::List(vec![
                        SvnItem::List(vec![
                            SvnItem::String(b"trunk/a.txt".to_vec()),
                            SvnItem::List(vec![SvnItem::Number(1)]),
                        ]),
                        SvnItem::List(vec![
                            SvnItem::String(b"trunk/b.txt".to_vec()),
                            SvnItem::List(Vec::new()),
                        ]),
                    ]),
                ]),
            ]);

            let expected_lock_a = SvnItem::List(vec![
                SvnItem::Word("lock".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/a.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
                    SvnItem::Bool(true),
                    SvnItem::List(vec![SvnItem::Number(1)]),
                ]),
            ]);

            let expected_lock_b = SvnItem::List(vec![
                SvnItem::Word("lock".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/b.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
                    SvnItem::Bool(true),
                    SvnItem::List(Vec::new()),
                ]),
            ]);

            let unknown_cmd_err = SvnItem::List(vec![
                SvnItem::Number(999),
                SvnItem::String(b"Unknown command".to_vec()),
                SvnItem::String(b"file".to_vec()),
                SvnItem::Number(1),
            ]);

            let lock_a = SvnItem::List(vec![
                SvnItem::String(b"trunk/a.txt".to_vec()),
                SvnItem::String(b"t1".to_vec()),
                SvnItem::String(b"alice".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::String(b"2025-01-01".to_vec()),
            ]);
            let lock_b = SvnItem::List(vec![
                SvnItem::String(b"trunk/b.txt".to_vec()),
                SvnItem::String(b"t2".to_vec()),
                SvnItem::String(b"alice".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::String(b"2025-01-01".to_vec()),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_lock_many)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("failure".to_string()),
                        SvnItem::List(vec![unknown_cmd_err]),
                    ]),
                )
                .await;

                assert_eq!(read_line(&mut server).await, encode_line(&expected_lock_a));
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![lock_a]),
                    ]),
                )
                .await;

                assert_eq!(read_line(&mut server).await, encode_line(&expected_lock_b));
                write_item_line(&mut server, &auth_request("realm-3")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![lock_b]),
                    ]),
                )
                .await;

                write_item_line(&mut server, &cmd_success).await;
            });

            let lock_results = session
                .lock_many(
                    &LockManyOptions::new().with_comment("hi").steal_lock(),
                    &[
                        LockTarget::new("trunk/a.txt").with_current_rev(1),
                        LockTarget::new("trunk/b.txt"),
                    ],
                )
                .await
                .unwrap();

            assert_eq!(lock_results.len(), 2);
            assert_eq!(lock_results[0].as_ref().unwrap().token, "t1");
            assert_eq!(lock_results[1].as_ref().unwrap().token, "t2");

            server_task.await.unwrap();
        });
    }

    #[test]
    fn unlock_many_falls_back_to_unlock_when_unsupported() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_unlock_many = SvnItem::List(vec![
                SvnItem::Word("unlock-many".to_string()),
                SvnItem::List(vec![
                    SvnItem::Bool(true),
                    SvnItem::List(vec![
                        SvnItem::List(vec![
                            SvnItem::String(b"trunk/a.txt".to_vec()),
                            SvnItem::List(vec![SvnItem::String(b"t1".to_vec())]),
                        ]),
                        SvnItem::List(vec![
                            SvnItem::String(b"trunk/b.txt".to_vec()),
                            SvnItem::List(Vec::new()),
                        ]),
                    ]),
                ]),
            ]);

            let expected_unlock_a = SvnItem::List(vec![
                SvnItem::Word("unlock".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/a.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::String(b"t1".to_vec())]),
                    SvnItem::Bool(true),
                ]),
            ]);

            let expected_unlock_b = SvnItem::List(vec![
                SvnItem::Word("unlock".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/b.txt".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(true),
                ]),
            ]);

            let unknown_cmd_err = SvnItem::List(vec![
                SvnItem::Number(999),
                SvnItem::String(b"Unknown command".to_vec()),
                SvnItem::String(b"file".to_vec()),
                SvnItem::Number(1),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_unlock_many)
                );
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("failure".to_string()),
                        SvnItem::List(vec![unknown_cmd_err]),
                    ]),
                )
                .await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_unlock_a)
                );
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(&mut server, &cmd_success).await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_unlock_b)
                );
                write_item_line(&mut server, &auth_request("realm-3")).await;
                write_item_line(&mut server, &cmd_success).await;
            });

            let unlock_results = session
                .unlock_many(
                    &UnlockManyOptions::new().break_lock(),
                    &[
                        UnlockTarget::new("trunk/a.txt").with_token("t1"),
                        UnlockTarget::new("trunk/b.txt"),
                    ],
                )
                .await
                .unwrap();

            assert_eq!(unlock_results.len(), 2);
            assert_eq!(unlock_results[0].as_ref().unwrap(), "trunk/a.txt");
            assert_eq!(unlock_results[1].as_ref().unwrap(), "trunk/b.txt");

            server_task.await.unwrap();
        });
    }

    #[test]
    fn commit_sends_editor_commands_and_parses_commit_info() {
        run_async(async {
            let (mut session, mut server) = connected_session().await;

            let expected_commit = SvnItem::List(vec![
                SvnItem::Word("commit".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"msg".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(false),
                    SvnItem::List(vec![SvnItem::List(vec![
                        SvnItem::String(b"svn:log".to_vec()),
                        SvnItem::String(b"msg".to_vec()),
                    ])]),
                ]),
            ]);

            let expected_open_root = SvnItem::List(vec![
                SvnItem::Word("open-root".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(vec![SvnItem::Number(1)]),
                    SvnItem::String(b"root".to_vec()),
                ]),
            ]);

            let expected_close_edit = SvnItem::List(vec![
                SvnItem::Word("close-edit".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let cmd_success = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(Vec::new()),
            ]);

            let commit_info = SvnItem::List(vec![
                SvnItem::Number(5),
                SvnItem::List(vec![SvnItem::String(b"2025-01-01".to_vec())]),
                SvnItem::List(vec![SvnItem::String(b"alice".to_vec())]),
            ]);

            let server_task = tokio::spawn(async move {
                assert_eq!(read_line(&mut server).await, encode_line(&expected_commit));
                write_item_line(&mut server, &auth_request("realm-1")).await;
                write_item_line(&mut server, &cmd_success).await;

                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_open_root)
                );
                assert_eq!(
                    read_line(&mut server).await,
                    encode_line(&expected_close_edit)
                );
                write_item_line(&mut server, &cmd_success).await;
                write_item_line(&mut server, &auth_request("realm-2")).await;
                write_item_line(&mut server, &commit_info).await;
            });

            let info = session
                .commit(
                    &CommitOptions::new("msg"),
                    &[
                        EditorCommand::OpenRoot {
                            rev: Some(1),
                            token: "root".to_string(),
                        },
                        EditorCommand::CloseEdit,
                    ],
                )
                .await
                .unwrap();

            server_task.await.unwrap();
            assert_eq!(info.new_rev, 5);
            assert_eq!(info.date.as_deref(), Some("2025-01-01"));
            assert_eq!(info.author.as_deref(), Some("alice"));
            assert_eq!(info.post_commit_err.as_deref(), None);
        });
    }
}
