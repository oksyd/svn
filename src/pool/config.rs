use super::*;

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
    pub(super) max_sessions: usize,
    pub(super) acquire_timeout: Option<Duration>,
    pub(super) idle_timeout: Option<Duration>,
    pub(super) health_check: SessionPoolHealthCheck,
    pub(super) prewarm_sessions: usize,
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
