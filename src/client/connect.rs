use super::*;

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
            reconnect_retries: 1,
            ra_client: "prototype-ra_svn".to_string(),
            #[cfg(feature = "ssh")]
            ssh: None,
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

    /// Returns the configured password, if any.
    pub(crate) fn password(&self) -> Option<&str> {
        self.password.as_deref()
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

    /// Sets how many times to reconnect and retry an operation on transient
    /// connection failures (for example `unexpected EOF`).
    ///
    /// This affects:
    /// - the initial handshake performed by [`RaSvnClient::open_session`];
    /// - per-operation reconnects performed by [`RaSvnSession`] methods that use
    ///   automatic retry.
    ///
    /// `0` disables retries (one attempt only). The default is `1`.
    #[must_use]
    pub fn with_reconnect_retries(mut self, retries: usize) -> Self {
        self.reconnect_retries = retries;
        self
    }

    /// Returns the configured reconnect retry count.
    pub fn reconnect_retries(&self) -> usize {
        self.reconnect_retries
    }

    /// Sets the SSH transport configuration for `svn+ssh://` URLs.
    ///
    /// This is ignored for `svn://` URLs.
    #[cfg(feature = "ssh")]
    #[must_use]
    pub fn with_ssh_config(mut self, ssh: crate::ssh::SshConfig) -> Self {
        self.ssh = Some(ssh);
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
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
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

    pub(super) async fn connect(&self) -> Result<(RaSvnConnection, ServerInfo), SvnError> {
        let is_tunneled = self
            .base_url
            .url
            .get(.."svn+ssh://".len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("svn+ssh://"));
        if is_tunneled {
            #[cfg(feature = "ssh")]
            {
                let ssh = self.ssh.clone().unwrap_or_default();
                let stream =
                    crate::ssh::open_svnserve_tunnel(&self.base_url, &ssh, self.connect_timeout)
                        .await?;
                return self.connect_over_stream(stream).await;
            }
            #[cfg(not(feature = "ssh"))]
            {
                return Err(SvnError::InvalidUrl(
                    "svn+ssh URLs require the crate feature `ssh`".to_string(),
                ));
            }
        }

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
                is_tunneled: false,
                ra_client: self.ra_client.clone(),
                read_timeout: self.read_timeout,
                write_timeout: self.write_timeout,
            },
        );
        let server_info = conn.handshake().await?;
        Ok((conn, server_info))
    }

    pub(super) async fn connect_over_stream<S>(
        &self,
        stream: S,
    ) -> Result<(RaSvnConnection, ServerInfo), SvnError>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + 'static,
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
                is_tunneled: self
                    .base_url
                    .url
                    .get(.."svn+ssh://".len())
                    .is_some_and(|prefix| prefix.eq_ignore_ascii_case("svn+ssh://")),
                ra_client: self.ra_client.clone(),
                read_timeout: self.read_timeout,
                write_timeout: self.write_timeout,
            },
        );
        let server_info = conn.handshake().await?;
        Ok((conn, server_info))
    }
}
