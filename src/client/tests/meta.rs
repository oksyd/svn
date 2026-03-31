use super::*;

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
fn get_latest_rev_reconnects_and_retries_on_unexpected_eof() {
    run_async(async {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_task = {
            let accepted = accepted.clone();
            tokio::spawn(async move {
                loop {
                    let (mut server, _) = listener.accept().await.unwrap();
                    let attempt = accepted.fetch_add(1, Ordering::SeqCst);

                    handshake_no_auth(&mut server).await;

                    let expected = SvnItem::List(vec![
                        SvnItem::Word("get-latest-rev".to_string()),
                        SvnItem::List(Vec::new()),
                    ]);
                    assert_eq!(read_line(&mut server).await, encode_line(&expected));
                    write_item_line(&mut server, &auth_request("realm")).await;

                    if attempt == 0 {
                        // Drop the connection before sending the command response to force an EOF.
                        continue;
                    }

                    write_item_line(
                        &mut server,
                        &SvnItem::List(vec![
                            SvnItem::Word("success".to_string()),
                            SvnItem::List(vec![SvnItem::Number(42)]),
                        ]),
                    )
                    .await;
                    break;
                }
            })
        };

        let url = SvnUrl::parse(&format!("svn://127.0.0.1:{}/repo", addr.port())).unwrap();
        let client = RaSvnClient::new(url, None, None)
            .with_connect_timeout(Duration::from_secs(1))
            .with_read_timeout(Duration::from_secs(1))
            .with_write_timeout(Duration::from_secs(1))
            .with_reconnect_retries(1);
        let mut session = client.open_session().await.unwrap();

        let rev = session.get_latest_rev().await.unwrap();
        assert_eq!(rev, 42);

        accepted_task.await.unwrap();
        assert_eq!(accepted.load(Ordering::SeqCst), 2);
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
