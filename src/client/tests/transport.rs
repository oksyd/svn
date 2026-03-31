use super::*;

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
