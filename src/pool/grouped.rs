use super::*;

/// A key used by [`SessionPools`] to partition pools by `host:port` and an
/// optional custom key.
#[derive(Clone, Eq, PartialEq, Hash)]
pub struct SessionPoolKey {
    host: String,
    port: u16,
    username: Option<String>,
    password: Option<String>,
    connect_timeout: Duration,
    read_timeout: Duration,
    write_timeout: Duration,
    ra_client: String,
    custom: Option<String>,
}

impl std::fmt::Debug for SessionPoolKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("SessionPoolKey");
        out.field("host", &self.host)
            .field("port", &self.port)
            .field("username", &self.username);
        if self.password.is_some() {
            out.field("password", &"<redacted>");
        } else {
            out.field("password", &None::<()>);
        }
        out.field("connect_timeout", &self.connect_timeout)
            .field("read_timeout", &self.read_timeout)
            .field("write_timeout", &self.write_timeout)
            .field("ra_client", &self.ra_client)
            .field("custom", &self.custom)
            .finish()
    }
}

impl SessionPoolKey {
    /// Creates a key from a client configuration (excluding the URL path).
    pub fn for_client(client: &RaSvnClient) -> Self {
        let url = client.base_url();
        Self {
            host: url.host.clone(),
            port: url.port,
            username: client.username().map(|s| s.to_string()),
            password: client.password().map(|s| s.to_string()),
            connect_timeout: client.connect_timeout(),
            read_timeout: client.read_timeout(),
            write_timeout: client.write_timeout(),
            ra_client: client.ra_client().to_string(),
            custom: None,
        }
    }

    /// Adds a custom partitioning key.
    #[must_use]
    pub fn with_custom(mut self, custom: impl Into<String>) -> Self {
        self.custom = Some(custom.into());
        self
    }
}

/// A map of [`SessionPool`] values partitioned by `host:port` and an optional key.
///
/// This is useful when your process needs to talk to multiple `svn://` servers
/// (or multiple independent tenants on the same server) while still reusing
/// connections and bounding concurrency.
#[derive(Clone)]
pub struct SessionPools {
    inner: Arc<SessionPoolsInner>,
}

impl std::fmt::Debug for SessionPools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionPools").finish()
    }
}

struct SessionPoolsInner {
    config: SessionPoolConfig,
    pools: Mutex<HashMap<SessionPoolKey, SessionPool>>,
}

impl SessionPools {
    /// Creates a new pool map.
    ///
    /// `config` is used for pools created on demand.
    pub fn new(config: SessionPoolConfig) -> Self {
        Self {
            inner: Arc::new(SessionPoolsInner {
                config,
                pools: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Returns (and creates if needed) a pool for `client`.
    pub fn pool(&self, client: RaSvnClient) -> Result<SessionPool, SvnError> {
        self.pool_inner(client, None)
    }

    /// Returns (and creates if needed) a pool for `client` partitioned by `key`.
    pub fn pool_with_key(
        &self,
        client: RaSvnClient,
        key: impl Into<String>,
    ) -> Result<SessionPool, SvnError> {
        self.pool_inner(client, Some(key.into()))
    }

    /// Checks out a session from a pool keyed by `client`.
    ///
    /// If the pooled session is connected to a different URL path on the same
    /// host, it is reparented before being returned.
    pub async fn session(&self, client: RaSvnClient) -> Result<PooledSession, SvnError> {
        self.session_inner(client, None).await
    }

    /// Checks out a session from a pool keyed by `client` and `key`.
    pub async fn session_with_key(
        &self,
        client: RaSvnClient,
        key: impl Into<String>,
    ) -> Result<PooledSession, SvnError> {
        self.session_inner(client, Some(key.into())).await
    }

    fn pool_inner(
        &self,
        client: RaSvnClient,
        key: Option<String>,
    ) -> Result<SessionPool, SvnError> {
        let mut pool_key = SessionPoolKey::for_client(&client);
        if let Some(key) = key {
            pool_key = pool_key.with_custom(key);
        }

        let mut pools = self
            .inner
            .pools
            .lock()
            .map_err(|_| SvnError::Protocol("session pools lock poisoned".into()))?;
        if let Some(pool) = pools.get(&pool_key) {
            return Ok(pool.clone());
        }

        let pool = SessionPool::with_config(client, self.inner.config.clone())?;
        pools.insert(pool_key, pool.clone());
        Ok(pool)
    }

    async fn session_inner(
        &self,
        client: RaSvnClient,
        key: Option<String>,
    ) -> Result<PooledSession, SvnError> {
        let base_url = client.base_url().clone();
        let pool = self.pool_inner(client, key)?;
        let mut session = pool.session().await?;
        if session.client().base_url().url != base_url.url {
            session.reparent(base_url).await?;
        }
        Ok(session)
    }
}
