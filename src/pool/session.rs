use super::*;

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

    #[cfg(test)]
    pub(crate) async fn acquire_permit_for_test(&self) -> Result<OwnedSemaphorePermit, SvnError> {
        self.inner.acquire_permit().await
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
