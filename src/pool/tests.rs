#![allow(clippy::unwrap_used)]

use super::*;
use crate::SvnUrl;
use crate::raw::SvnItem;
use crate::test_support::{read_until_newline, run_async, write_item_line};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
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
                    match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
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
                    match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
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
        assert_send_future(pool.acquire_permit_for_test());

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
                    match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
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
                    match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
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
                    match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
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
                    match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
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
                    match tokio::time::timeout(Duration::from_millis(50), listener.accept()).await {
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
