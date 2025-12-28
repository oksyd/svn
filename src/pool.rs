use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::{RaSvnClient, RaSvnSession, SvnError};

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
        if max_sessions == 0 {
            return Err(SvnError::Protocol("max_sessions must be > 0".into()));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                client,
                idle: Mutex::new(Vec::new()),
                semaphore: Arc::new(Semaphore::new(max_sessions)),
                max_sessions,
            }),
        })
    }

    /// Returns the maximum number of concurrent sessions.
    pub fn max_sessions(&self) -> usize {
        self.inner.max_sessions
    }

    /// Checks out a session from the pool.
    ///
    /// The returned [`PooledSession`] returns to the pool when dropped.
    pub async fn session(&self) -> Result<PooledSession, SvnError> {
        let permit = self
            .inner
            .semaphore
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| SvnError::Protocol("session pool closed".into()))?;

        let session = match self.inner.idle.lock() {
            Ok(mut idle) => idle.pop(),
            Err(_) => None,
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
}

struct Inner {
    client: RaSvnClient,
    idle: Mutex<Vec<RaSvnSession>>,
    semaphore: Arc<Semaphore>,
    max_sessions: usize,
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
        if let Some(session) = self.session.take()
            && let Ok(mut idle) = self.inner.idle.lock()
        {
            idle.push(session);
        }

        // Release the permit after returning to the pool, so waiters can reuse
        // this session instead of opening a new connection.
        let _permit = self.permit.take();
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

            let in_flight = Arc::new(AtomicUsize::new(0));
            let max_observed = Arc::new(AtomicUsize::new(0));

            let mut tasks = Vec::new();
            for _ in 0..6usize {
                let pool = pool.clone();
                let in_flight = in_flight.clone();
                let max_observed = max_observed.clone();
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
}
