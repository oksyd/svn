use super::*;

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
            revprops: LogRevProps::Custom(vec!["svn:author".to_string(), "svn:custom".to_string()]),
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
fn log_each_retrying_reconnects_and_dedups_on_unexpected_eof() {
    run_async(async {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let options = LogOptions {
            target_paths: vec!["trunk".to_string()],
            start_rev: Some(2),
            end_rev: Some(1),
            changed_paths: false,
            strict_node: true,
            limit: 0,
            include_merged_revisions: false,
            revprops: LogRevProps::Custom(vec![
                "svn:author".to_string(),
                "svn:date".to_string(),
                "svn:log".to_string(),
            ]),
        };

        let expected_log = SvnItem::List(vec![
            SvnItem::Word("log".to_string()),
            SvnItem::List(vec![
                SvnItem::List(vec![SvnItem::String(b"trunk".to_vec())]),
                SvnItem::List(vec![SvnItem::Number(2)]),
                SvnItem::List(vec![SvnItem::Number(1)]),
                SvnItem::Bool(false),
                SvnItem::Bool(true),
                SvnItem::Number(0),
                SvnItem::Bool(false),
                SvnItem::Word("revprops".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"svn:author".to_vec()),
                    SvnItem::String(b"svn:date".to_vec()),
                    SvnItem::String(b"svn:log".to_vec()),
                ]),
            ]),
        ]);

        let entry_10 = SvnItem::List(vec![
            SvnItem::List(Vec::new()),
            SvnItem::Number(10),
            SvnItem::List(vec![SvnItem::String(b"alice".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"2025-01-01".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"msg".to_vec())]),
        ]);
        let entry_9 = SvnItem::List(vec![
            SvnItem::List(Vec::new()),
            SvnItem::Number(9),
            SvnItem::List(vec![SvnItem::String(b"bob".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"2025-01-02".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"msg2".to_vec())]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let accepted = Arc::new(AtomicUsize::new(0));
        let accepted_task = {
            let accepted = accepted.clone();
            tokio::spawn(async move {
                loop {
                    let (mut server, _) = listener.accept().await.unwrap();
                    let attempt = accepted.fetch_add(1, Ordering::SeqCst);

                    handshake_no_auth(&mut server).await;

                    assert_eq!(read_line(&mut server).await, encode_line(&expected_log));
                    write_item_line(&mut server, &auth_request("realm")).await;

                    write_item_line(&mut server, &entry_10).await;

                    if attempt == 0 {
                        // Drop mid-stream.
                        continue;
                    }

                    write_item_line(&mut server, &entry_9).await;
                    write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
                    write_item_line(&mut server, &cmd_success).await;
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

        let mut revs = Vec::new();
        session
            .log_each_retrying(&options, |entry| {
                revs.push(entry.rev);
                Ok(())
            })
            .await
            .unwrap();

        assert_eq!(revs, vec![10, 9]);

        accepted_task.await.unwrap();
        assert_eq!(accepted.load(Ordering::SeqCst), 2);
    });
}
