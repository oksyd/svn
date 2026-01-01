use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{RaSvnClient, RaSvnSession, SvnError};

/// Health check behavior for [`SessionPool`] idle sessions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionPoolHealthCheck {
    /// Never perform a health check.
    None,
    /// Always health-check idle sessions when checking them out.
    OnCheckout,
    /// Health-check idle sessions only if they were idle for at least `Duration`.
    OnCheckoutIfIdleFor(Duration),
}

/// Configuration for [`SessionPool`].
#[derive(Clone, Debug)]
pub struct SessionPoolConfig {
    max_sessions: usize,
    acquire_timeout: Option<Duration>,
    idle_timeout: Option<Duration>,
    health_check: SessionPoolHealthCheck,
    prewarm_sessions: usize,
}

impl SessionPoolConfig {
    /// Creates a new config with `max_sessions` and no timeouts.
    pub fn new(max_sessions: usize) -> Result<Self, SvnError> {
        if max_sessions == 0 {
            return Err(SvnError::Protocol("max_sessions must be > 0".into()));
        }
        Ok(Self {
            max_sessions,
            acquire_timeout: None,
            idle_timeout: None,
            health_check: SessionPoolHealthCheck::None,
            prewarm_sessions: 0,
        })
    }

    /// Returns the maximum number of concurrent sessions.
    pub fn max_sessions(&self) -> usize {
        self.max_sessions
    }

    /// Sets a timeout for [`SessionPool::session`] when waiting for capacity.
    #[must_use]
    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = Some(timeout);
        self
    }

    /// Sets an idle timeout after which sessions are dropped.
    #[must_use]
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = Some(timeout);
        self
    }

    /// Configures health checks for idle sessions.
    #[must_use]
    pub fn with_health_check(mut self, health_check: SessionPoolHealthCheck) -> Self {
        self.health_check = health_check;
        self
    }

    /// Prewarms up to `sessions` idle connections when calling [`SessionPool::warm_up`].
    ///
    /// Values larger than `max_sessions` are clamped.
    #[must_use]
    pub fn with_prewarm_sessions(mut self, sessions: usize) -> Self {
        self.prewarm_sessions = sessions;
        self
    }
}

/// A bounded pool of connected [`RaSvnSession`] values.
///
/// `ra_svn` sessions are stateful and require `&mut` access, which makes a single
/// session inherently serial. If you want to run multiple independent
/// operations concurrently, you need multiple sessions (and therefore multiple
/// TCP connections).
///
/// `SessionPool` manages those sessions for you:
/// - Limits concurrency via `max_sessions`.
/// - Reuses sessions (and their handshake) across operations.
/// - Creates new sessions on demand up to the limit.
///
/// # Example
///
/// ```rust,no_run
/// # use svn::{RaSvnClient, SessionPool, SvnUrl};
/// # async fn demo() -> svn::Result<()> {
/// let client = RaSvnClient::new(SvnUrl::parse("svn://example.com/repo")?, None, None);
/// let pool = SessionPool::new(client, 8)?;
///
/// let rev = {
///     let mut session = pool.session().await?;
///     session.get_latest_rev().await?
/// };
/// # Ok(())
/// # }
/// ```
#[derive(Clone)]
pub struct SessionPool {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for SessionPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionPool")
            .field("max_sessions", &self.max_sessions())
            .finish()
    }
}

impl SessionPool {
    /// Creates a new session pool for `client` with a maximum of `max_sessions`
    /// checked out concurrently.
    pub fn new(client: RaSvnClient, max_sessions: usize) -> Result<Self, SvnError> {
        Self::with_config(client, SessionPoolConfig::new(max_sessions)?)
    }

    /// Creates a new session pool with an explicit [`SessionPoolConfig`].
    pub fn with_config(client: RaSvnClient, config: SessionPoolConfig) -> Result<Self, SvnError> {
        let max_sessions = config.max_sessions;
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                config,
                idle: Mutex::new(Vec::new()),
                semaphore: Arc::new(Semaphore::new(max_sessions)),
            }),
        })
    }

    /// Returns the maximum number of concurrent sessions.
    pub fn max_sessions(&self) -> usize {
        self.inner.config.max_sessions
    }

    /// Returns a copy of the pool configuration.
    pub fn config(&self) -> SessionPoolConfig {
        self.inner.config.clone()
    }

    /// Prewarms idle connections up to the configured `prewarm_sessions`.
    pub async fn warm_up(&self) -> Result<usize, SvnError> {
        self.warm_up_to(self.inner.config.prewarm_sessions).await
    }

    /// Prewarms idle connections up to `target_idle`.
    pub async fn warm_up_to(&self, target_idle: usize) -> Result<usize, SvnError> {
        let target_idle = target_idle.min(self.inner.config.max_sessions);
        if target_idle == 0 {
            return Ok(0);
        }

        let mut created = 0usize;
        let mut sessions = Vec::new();
        let mut permits = Vec::new();

        loop {
            let idle_len = match self.inner.idle.lock() {
                Ok(idle) => idle.len(),
                Err(_) => 0,
            };
            if idle_len + sessions.len() >= target_idle {
                break;
            }

            let permit = self.inner.acquire_permit().await?;
            let session = self.inner.client.open_session().await?;
            permits.push(permit);
            sessions.push(session);
            created += 1;
        }

        if !sessions.is_empty() {
            let now = Instant::now();
            if let Ok(mut idle) = self.inner.idle.lock() {
                for session in sessions {
                    idle.push(IdleSession {
                        session,
                        idle_since: now,
                    });
                }
            }
        }

        // Drop permits after returning sessions to idle so waiters can reuse them.
        drop(permits);

        Ok(created)
    }

    /// Checks out a session from the pool.
    ///
    /// The returned [`PooledSession`] returns to the pool when dropped.
    pub async fn session(&self) -> Result<PooledSession, SvnError> {
        let permit = self.inner.acquire_permit().await?;

        let now = Instant::now();
        let session = loop {
            let entry = self.inner.pop_idle_session();

            let Some(entry) = entry else {
                break None;
            };

            if let Some(timeout) = self.inner.config.idle_timeout
                && now.saturating_duration_since(entry.idle_since) >= timeout
            {
                continue;
            }

            let idle_for = now.saturating_duration_since(entry.idle_since);
            let mut session = entry.session;
            let should_check = match self.inner.config.health_check {
                SessionPoolHealthCheck::None => false,
                SessionPoolHealthCheck::OnCheckout => true,
                SessionPoolHealthCheck::OnCheckoutIfIdleFor(min_idle) => idle_for >= min_idle,
            };
            if should_check && session.get_latest_rev().await.is_err() {
                continue;
            }

            break Some(session);
        };

        let session = match session {
            Some(session) => session,
            None => self.inner.client.open_session().await?,
        };

        Ok(PooledSession {
            inner: self.inner.clone(),
            session: Some(session),
            permit: Some(permit),
        })
    }
}

impl RaSvnClient {
    /// Creates a [`SessionPool`] using this client configuration.
    pub fn session_pool(&self, max_sessions: usize) -> Result<SessionPool, SvnError> {
        SessionPool::new(self.clone(), max_sessions)
    }

    /// Creates a [`SessionPool`] using this client configuration and `config`.
    pub fn session_pool_with_config(
        &self,
        config: SessionPoolConfig,
    ) -> Result<SessionPool, SvnError> {
        SessionPool::with_config(self.clone(), config)
    }
}

struct Inner {
    client: RaSvnClient,
    config: SessionPoolConfig,
    idle: Mutex<Vec<IdleSession>>,
    semaphore: Arc<Semaphore>,
}

impl Inner {
    async fn acquire_permit(&self) -> Result<OwnedSemaphorePermit, SvnError> {
        let fut = self.semaphore.clone().acquire_owned();
        if let Some(timeout) = self.config.acquire_timeout {
            match tokio::time::timeout(timeout, fut).await {
                Ok(permit) => permit.map_err(|_| SvnError::Protocol("session pool closed".into())),
                Err(_) => Err(SvnError::Protocol("session pool acquire timed out".into())),
            }
        } else {
            fut.await
                .map_err(|_| SvnError::Protocol("session pool closed".into()))
        }
    }

    fn pop_idle_session(&self) -> Option<IdleSession> {
        self.idle.lock().ok().and_then(|mut idle| idle.pop())
    }

    fn push_idle_session(&self, session: RaSvnSession) {
        if let Ok(mut idle) = self.idle.lock() {
            idle.push(IdleSession {
                session,
                idle_since: Instant::now(),
            });
        }
    }
}

#[derive(Debug)]
struct IdleSession {
    session: RaSvnSession,
    idle_since: Instant,
}

/// A checked-out session returned by [`SessionPool::session`].
///
/// When dropped, the session is returned to its originating pool.
pub struct PooledSession {
    inner: Arc<Inner>,
    session: Option<RaSvnSession>,
    permit: Option<OwnedSemaphorePermit>,
}

impl std::fmt::Debug for PooledSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PooledSession").finish()
    }
}

impl Deref for PooledSession {
    type Target = RaSvnSession;

    #[allow(clippy::panic)]
    fn deref(&self) -> &Self::Target {
        match self.session.as_ref() {
            Some(session) => session,
            None => {
                panic!("pooled session missing inner value");
            }
        }
    }
}

impl DerefMut for PooledSession {
    #[allow(clippy::panic)]
    fn deref_mut(&mut self) -> &mut Self::Target {
        match self.session.as_mut() {
            Some(session) => session,
            None => {
                panic!("pooled session missing inner value");
            }
        }
    }
}

impl Drop for PooledSession {
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            self.inner.push_idle_session(session);
        }

        // Release the permit after returning to the pool, so waiters can reuse
        // this session instead of opening a new connection.
        let _permit = self.permit.take();
    }
}

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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crate::SvnUrl;
    use crate::rasvn::encode_item;
    use crate::raw::SvnItem;
    use std::future::Future;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    fn run_async<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    async fn write_item_line(stream: &mut tokio::net::TcpStream, item: &SvnItem) {
        let mut buf = Vec::new();
        encode_item(item, &mut buf);
        buf.push(b'\n');
        stream.write_all(&buf).await.unwrap();
        stream.flush().await.unwrap();
    }

    async fn read_until_newline(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
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

    async fn handle_handshake(mut stream: tokio::net::TcpStream) {
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
        write_item_line(&mut stream, &greeting).await;

        let _client_greeting = read_until_newline(&mut stream).await;

        let auth_request = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(vec![
                SvnItem::List(Vec::new()),
                SvnItem::String(b"realm".to_vec()),
            ]),
        ]);
        write_item_line(&mut stream, &auth_request).await;

        let repos_info = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"uuid".to_vec()),
                SvnItem::String(b"svn://example.com/repo".to_vec()),
                SvnItem::List(Vec::new()),
            ]),
        ]);
        write_item_line(&mut stream, &repos_info).await;

        // Keep the connection open until the client closes it.
        let mut tmp = [0u8; 1024];
        loop {
            let n = stream.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
        }
    }

    #[test]
    fn session_pool_reuses_sessions_and_limits_connections() {
        run_async(async {
            fn assert_send<T: Send>() {}
            fn assert_sync<T: Sync>() {}
            assert_send::<SessionPool>();
            assert_sync::<SessionPool>();
            assert_send::<PooledSession>();
            assert_send::<RaSvnSession>();
            assert_send::<OwnedSemaphorePermit>();

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            let accepted = Arc::new(AtomicUsize::new(0));
            let done = Arc::new(AtomicBool::new(false));
            let accepted_task = {
                let accepted = accepted.clone();
                let done = done.clone();
                tokio::spawn(async move {
                    while !done.load(Ordering::SeqCst) {
                        match tokio::time::timeout(Duration::from_millis(50), listener.accept())
                            .await
                        {
                            Ok(Ok((stream, _))) => {
                                accepted.fetch_add(1, Ordering::SeqCst);
                                tokio::spawn(handle_handshake(stream));
                            }
                            Ok(Err(_)) => break,
                            Err(_) => continue,
                        }
                    }
                })
            };

            let url = SvnUrl::parse(&format!("svn://127.0.0.1:{}/repo", addr.port())).unwrap();
            let client = RaSvnClient::new(url, None, None);
            let pool = SessionPool::new(client, 1).unwrap();

            // Sequential checkouts should reuse the same connection.
            for _ in 0..5usize {
                let _session = pool.session().await.unwrap();
                tokio::time::sleep(Duration::from_millis(10)).await;
            }

            drop(pool);
            done.store(true, Ordering::SeqCst);
            accepted_task.await.unwrap();

            assert_eq!(accepted.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn session_pool_enforces_max_sessions() {
        run_async(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            let accepted = Arc::new(AtomicUsize::new(0));
            let done = Arc::new(AtomicBool::new(false));
            let accepted_task = {
                let accepted = accepted.clone();
                let done = done.clone();
                tokio::spawn(async move {
                    while !done.load(Ordering::SeqCst) {
                        match tokio::time::timeout(Duration::from_millis(50), listener.accept())
                            .await
                        {
                            Ok(Ok((stream, _))) => {
                                accepted.fetch_add(1, Ordering::SeqCst);
                                tokio::spawn(handle_handshake(stream));
                            }
                            Ok(Err(_)) => break,
                            Err(_) => continue,
                        }
                    }
                })
            };

            let url = SvnUrl::parse(&format!("svn://127.0.0.1:{}/repo", addr.port())).unwrap();
            let client = RaSvnClient::new(url, None, None);
            let pool = SessionPool::new(client, 2).unwrap();

            fn assert_send_future<F: Future + Send>(_: F) {}
            assert_send_future(pool.inner.acquire_permit());

            let in_flight = Arc::new(AtomicUsize::new(0));
            let max_observed = Arc::new(AtomicUsize::new(0));

            let mut tasks = Vec::new();
            for _ in 0..6usize {
                let pool = pool.clone();
                let in_flight = in_flight.clone();
                let max_observed = max_observed.clone();

                fn assert_send_future<F: Future + Send>(_: F) {}
                assert_send_future(pool.session());

                tasks.push(tokio::spawn(async move {
                    let _session = pool.session().await.unwrap();
                    let cur = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                    max_observed.fetch_max(cur, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    drop(_session);
                    in_flight.fetch_sub(1, Ordering::SeqCst);
                }));
            }

            for t in tasks {
                t.await.unwrap();
            }

            drop(pool);
            done.store(true, Ordering::SeqCst);
            accepted_task.await.unwrap();

            assert_eq!(max_observed.load(Ordering::SeqCst), 2);
            assert_eq!(accepted.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    fn session_pool_drops_idle_sessions_after_timeout() {
        run_async(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            let accepted = Arc::new(AtomicUsize::new(0));
            let done = Arc::new(AtomicBool::new(false));
            let accepted_task = {
                let accepted = accepted.clone();
                let done = done.clone();
                tokio::spawn(async move {
                    while !done.load(Ordering::SeqCst) {
                        match tokio::time::timeout(Duration::from_millis(50), listener.accept())
                            .await
                        {
                            Ok(Ok((stream, _))) => {
                                accepted.fetch_add(1, Ordering::SeqCst);
                                tokio::spawn(handle_handshake(stream));
                            }
                            Ok(Err(_)) => break,
                            Err(_) => continue,
                        }
                    }
                })
            };

            let url = SvnUrl::parse(&format!("svn://127.0.0.1:{}/repo", addr.port())).unwrap();
            let client = RaSvnClient::new(url, None, None);
            let config = SessionPoolConfig::new(1)
                .unwrap()
                .with_idle_timeout(Duration::from_millis(20));
            let pool = SessionPool::with_config(client, config).unwrap();

            let _session = pool.session().await.unwrap();
            drop(_session);

            tokio::time::sleep(Duration::from_millis(30)).await;

            let _session = pool.session().await.unwrap();
            drop(_session);

            drop(pool);
            done.store(true, Ordering::SeqCst);
            accepted_task.await.unwrap();

            assert_eq!(accepted.load(Ordering::SeqCst), 2);
        });
    }

    #[test]
    fn session_pool_acquire_timeout_errors_when_at_capacity() {
        run_async(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            let done = Arc::new(AtomicBool::new(false));
            let accepted_task = {
                let done = done.clone();
                tokio::spawn(async move {
                    while !done.load(Ordering::SeqCst) {
                        match tokio::time::timeout(Duration::from_millis(50), listener.accept())
                            .await
                        {
                            Ok(Ok((stream, _))) => {
                                tokio::spawn(handle_handshake(stream));
                            }
                            Ok(Err(_)) => break,
                            Err(_) => continue,
                        }
                    }
                })
            };

            let url = SvnUrl::parse(&format!("svn://127.0.0.1:{}/repo", addr.port())).unwrap();
            let client = RaSvnClient::new(url, None, None);
            let config = SessionPoolConfig::new(1)
                .unwrap()
                .with_acquire_timeout(Duration::from_millis(20));
            let pool = SessionPool::with_config(client, config).unwrap();

            let session = pool.session().await.unwrap();
            let err = pool.session().await.unwrap_err();
            assert!(matches!(err, SvnError::Protocol(_)));
            drop(session);

            drop(pool);
            done.store(true, Ordering::SeqCst);
            accepted_task.await.unwrap();
        });
    }

    #[test]
    fn session_pool_warm_up_opens_expected_number_of_connections() {
        run_async(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            let accepted = Arc::new(AtomicUsize::new(0));
            let done = Arc::new(AtomicBool::new(false));
            let accepted_task = {
                let accepted = accepted.clone();
                let done = done.clone();
                tokio::spawn(async move {
                    while !done.load(Ordering::SeqCst) {
                        match tokio::time::timeout(Duration::from_millis(50), listener.accept())
                            .await
                        {
                            Ok(Ok((stream, _))) => {
                                accepted.fetch_add(1, Ordering::SeqCst);
                                tokio::spawn(handle_handshake(stream));
                            }
                            Ok(Err(_)) => break,
                            Err(_) => continue,
                        }
                    }
                })
            };

            let url = SvnUrl::parse(&format!("svn://127.0.0.1:{}/repo", addr.port())).unwrap();
            let client = RaSvnClient::new(url, None, None);
            let config = SessionPoolConfig::new(4).unwrap().with_prewarm_sessions(3);
            let pool = SessionPool::with_config(client, config).unwrap();

            assert_eq!(pool.warm_up().await.unwrap(), 3);

            drop(pool);
            done.store(true, Ordering::SeqCst);
            accepted_task.await.unwrap();

            assert_eq!(accepted.load(Ordering::SeqCst), 3);
        });
    }

    #[test]
    fn session_pool_health_checks_idle_sessions_on_checkout() {
        run_async(async {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            let check_count = Arc::new(AtomicUsize::new(0));
            let done = Arc::new(AtomicBool::new(false));
            let accepted_task = {
                let done = done.clone();
                let check_count = check_count.clone();
                tokio::spawn(async move {
                    while !done.load(Ordering::SeqCst) {
                        match tokio::time::timeout(Duration::from_millis(50), listener.accept())
                            .await
                        {
                            Ok(Ok((mut stream, _))) => {
                                let check_count = check_count.clone();
                                tokio::spawn(async move {
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
                                    write_item_line(&mut stream, &greeting).await;
                                    let _client_greeting = read_until_newline(&mut stream).await;

                                    let auth_request = SvnItem::List(vec![
                                        SvnItem::Word("success".to_string()),
                                        SvnItem::List(vec![
                                            SvnItem::List(Vec::new()),
                                            SvnItem::String(b"realm".to_vec()),
                                        ]),
                                    ]);
                                    write_item_line(&mut stream, &auth_request).await;

                                    let repos_info = SvnItem::List(vec![
                                        SvnItem::Word("success".to_string()),
                                        SvnItem::List(vec![
                                            SvnItem::String(b"uuid".to_vec()),
                                            SvnItem::String(b"svn://example.com/repo".to_vec()),
                                            SvnItem::List(Vec::new()),
                                        ]),
                                    ]);
                                    write_item_line(&mut stream, &repos_info).await;

                                    loop {
                                        let line = read_until_newline(&mut stream).await;
                                        if line.is_empty() {
                                            break;
                                        }

                                        check_count.fetch_add(1, Ordering::SeqCst);
                                        write_item_line(&mut stream, &auth_request).await;
                                        let latest = SvnItem::List(vec![
                                            SvnItem::Word("success".to_string()),
                                            SvnItem::List(vec![SvnItem::Number(123)]),
                                        ]);
                                        write_item_line(&mut stream, &latest).await;
                                    }
                                });
                            }
                            Ok(Err(_)) => break,
                            Err(_) => continue,
                        }
                    }
                })
            };

            let url = SvnUrl::parse(&format!("svn://127.0.0.1:{}/repo", addr.port())).unwrap();
            let client = RaSvnClient::new(url, None, None);
            let config = SessionPoolConfig::new(1)
                .unwrap()
                .with_health_check(SessionPoolHealthCheck::OnCheckout);
            let pool = SessionPool::with_config(client, config).unwrap();

            let session = pool.session().await.unwrap();
            drop(session);

            let session = pool.session().await.unwrap();
            drop(session);

            drop(pool);
            done.store(true, Ordering::SeqCst);
            accepted_task.await.unwrap();

            assert_eq!(check_count.load(Ordering::SeqCst), 1);
        });
    }

    #[test]
    fn session_pools_partitions_by_custom_key() {
        run_async(async {
            fn assert_send<T: Send>() {}
            fn assert_sync<T: Sync>() {}
            assert_send::<SessionPools>();
            assert_sync::<SessionPools>();
            assert_send::<PooledSession>();

            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();

            let accepted = Arc::new(AtomicUsize::new(0));
            let done = Arc::new(AtomicBool::new(false));
            let accepted_task = {
                let accepted = accepted.clone();
                let done = done.clone();
                tokio::spawn(async move {
                    while !done.load(Ordering::SeqCst) {
                        match tokio::time::timeout(Duration::from_millis(50), listener.accept())
                            .await
                        {
                            Ok(Ok((stream, _))) => {
                                accepted.fetch_add(1, Ordering::SeqCst);
                                tokio::spawn(handle_handshake(stream));
                            }
                            Ok(Err(_)) => break,
                            Err(_) => continue,
                        }
                    }
                })
            };

            let url = SvnUrl::parse(&format!("svn://127.0.0.1:{}/repo", addr.port())).unwrap();
            let client = RaSvnClient::new(url, None, None);

            let pools = SessionPools::new(SessionPoolConfig::new(1).unwrap());

            let mut tasks = Vec::new();
            for key in ["a", "b"] {
                let pools = pools.clone();
                let client = client.clone();

                fn assert_send_future<F: Future + Send>(_: F) {}
                assert_send_future(pools.session_with_key(client.clone(), key));

                tasks.push(tokio::spawn(async move {
                    let _session = pools.session_with_key(client, key).await.unwrap();
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }));
            }

            for task in tasks {
                task.await.unwrap();
            }

            drop(pools);
            done.store(true, Ordering::SeqCst);
            accepted_task.await.unwrap();

            assert_eq!(accepted.load(Ordering::SeqCst), 2);
        });
    }
}
