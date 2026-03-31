use super::*;

#[test]
fn list_dir_sends_expected_get_dir_params_and_parses_entries() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_get_dir = SvnItem::List(vec![
            SvnItem::Word("get-dir".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::Bool(false), // want-props
                SvnItem::Bool(true),  // want-contents
                SvnItem::List(vec![
                    SvnItem::Word("kind".to_string()),
                    SvnItem::Word("size".to_string()),
                    SvnItem::Word("has-props".to_string()),
                    SvnItem::Word("created-rev".to_string()),
                    SvnItem::Word("time".to_string()),
                    SvnItem::Word("last-author".to_string()),
                ]),
                SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
            ]),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_get_dir));
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::Number(9),
                        SvnItem::List(Vec::new()),
                        SvnItem::List(vec![SvnItem::List(vec![
                            SvnItem::String(b"file.txt".to_vec()),
                            SvnItem::Word("file".to_string()),
                            SvnItem::Number(3),
                            SvnItem::Bool(false),
                            SvnItem::Number(9),
                        ])]),
                    ]),
                ]),
            )
            .await;
        });

        let listing = session.list_dir("trunk", None).await.unwrap();
        assert_eq!(listing.rev, 9);
        assert_eq!(listing.entries.len(), 1);
        assert_eq!(listing.entries[0].name, "file.txt");
        assert_eq!(listing.entries[0].path, "trunk/file.txt");
        assert_eq!(listing.entries[0].kind, NodeKind::File);
        assert_eq!(listing.entries[0].size, Some(3));

        server_task.await.unwrap();
    });
}

#[test]
fn list_sends_patterns_and_reads_dirents_until_done() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_list = SvnItem::List(vec![
            SvnItem::Word("list".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
                SvnItem::List(vec![SvnItem::Word("kind".to_string())]),
                SvnItem::List(vec![SvnItem::String(b"*.rs".to_vec())]),
            ]),
        ]);

        let dirent = SvnItem::List(vec![
            SvnItem::String(b"trunk/main.rs".to_vec()),
            SvnItem::Word("file".to_string()),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_list));
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(&mut server, &dirent).await;
            write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let patterns = vec![String::from("*.rs")];
        let fields = [DirentField::Kind];
        let entries = session
            .list(
                "trunk",
                None,
                Depth::Infinity,
                &fields,
                Some(patterns.as_slice()),
            )
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "trunk/main.rs");
        assert_eq!(entries[0].name, "main.rs");
        assert_eq!(entries[0].kind, NodeKind::File);

        server_task.await.unwrap();
    });
}

#[test]
fn list_recursive_falls_back_to_get_dir_when_list_cap_missing() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_get_dir_trunk = SvnItem::List(vec![
            SvnItem::Word("get-dir".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::Bool(false), // want-props
                SvnItem::Bool(true),  // want-contents
                SvnItem::List(vec![
                    SvnItem::Word("kind".to_string()),
                    SvnItem::Word("size".to_string()),
                    SvnItem::Word("has-props".to_string()),
                    SvnItem::Word("created-rev".to_string()),
                    SvnItem::Word("time".to_string()),
                    SvnItem::Word("last-author".to_string()),
                ]),
                SvnItem::Bool(false), // want-iprops
            ]),
        ]);

        let expected_get_dir_sub = SvnItem::List(vec![
            SvnItem::Word("get-dir".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/sub".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::Bool(false), // want-props
                SvnItem::Bool(true),  // want-contents
                SvnItem::List(vec![
                    SvnItem::Word("kind".to_string()),
                    SvnItem::Word("size".to_string()),
                    SvnItem::Word("has-props".to_string()),
                    SvnItem::Word("created-rev".to_string()),
                    SvnItem::Word("time".to_string()),
                    SvnItem::Word("last-author".to_string()),
                ]),
                SvnItem::Bool(false), // want-iprops
            ]),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_dir_trunk)
            );
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::Number(9),
                        SvnItem::List(Vec::new()),
                        SvnItem::List(vec![
                            SvnItem::List(vec![
                                SvnItem::String(b"sub".to_vec()),
                                SvnItem::Word("dir".to_string()),
                                SvnItem::Number(0),
                                SvnItem::Bool(false),
                                SvnItem::Number(9),
                            ]),
                            SvnItem::List(vec![
                                SvnItem::String(b"a.txt".to_vec()),
                                SvnItem::Word("file".to_string()),
                                SvnItem::Number(1),
                                SvnItem::Bool(false),
                                SvnItem::Number(9),
                            ]),
                        ]),
                    ]),
                ]),
            )
            .await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_dir_sub)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::Number(9),
                        SvnItem::List(Vec::new()),
                        SvnItem::List(vec![SvnItem::List(vec![
                            SvnItem::String(b"b.txt".to_vec()),
                            SvnItem::Word("file".to_string()),
                            SvnItem::Number(2),
                            SvnItem::Bool(false),
                            SvnItem::Number(9),
                        ])]),
                    ]),
                ]),
            )
            .await;
        });

        let mut entries = session.list_recursive("trunk", None).await.unwrap();
        entries.sort_by(|a, b| a.path.cmp(&b.path));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "trunk/a.txt");
        assert_eq!(entries[1].path, "trunk/sub");
        assert_eq!(entries[2].path, "trunk/sub/b.txt");

        server_task.await.unwrap();
    });
}

#[test]
fn list_recursive_uses_list_capability_when_available() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;
        session
            .conn
            .as_mut()
            .unwrap()
            .set_server_caps_for_test(&["list"]);

        let expected_list = SvnItem::List(vec![
            SvnItem::Word("list".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
                SvnItem::List(vec![
                    SvnItem::Word("kind".to_string()),
                    SvnItem::Word("size".to_string()),
                    SvnItem::Word("has-props".to_string()),
                    SvnItem::Word("created-rev".to_string()),
                    SvnItem::Word("time".to_string()),
                    SvnItem::Word("last-author".to_string()),
                ]),
            ]),
        ]);

        let dirent = SvnItem::List(vec![
            SvnItem::String(b"trunk/main.rs".to_vec()),
            SvnItem::Word("file".to_string()),
            SvnItem::Number(3),
            SvnItem::Bool(false),
            SvnItem::Number(9),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_list));
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(&mut server, &dirent).await;
            write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let entries = session.list_recursive("trunk", None).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "trunk/main.rs");
        assert_eq!(entries[0].kind, NodeKind::File);

        server_task.await.unwrap();
    });
}

#[test]
fn stat_returns_none_when_check_path_reports_none() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_stat = SvnItem::List(vec![
            SvnItem::Word("stat".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/missing.txt".to_vec()),
                SvnItem::List(Vec::new()),
            ]),
        ]);

        let expected_check_path = SvnItem::List(vec![
            SvnItem::Word("check-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/missing.txt".to_vec()),
                SvnItem::List(Vec::new()),
            ]),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_stat));
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_check_path)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::Word("none".to_string())]),
                ]),
            )
            .await;
        });

        let stat = session.stat("trunk/missing.txt", None).await.unwrap();
        assert_eq!(stat, None);

        server_task.await.unwrap();
    });
}

#[test]
fn get_mergeinfo_sends_command_and_parses_catalog() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_mergeinfo = SvnItem::List(vec![
            SvnItem::Word("get-mergeinfo".to_string()),
            SvnItem::List(vec![
                SvnItem::List(vec![SvnItem::String(b"trunk".to_vec())]),
                SvnItem::List(Vec::new()),
                SvnItem::Word("explicit".to_string()),
                SvnItem::Bool(false),
            ]),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_mergeinfo)
            );
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::List(vec![SvnItem::List(vec![
                        SvnItem::String(b"/trunk".to_vec()),
                        SvnItem::String(b"/trunk:1-2".to_vec()),
                    ])])]),
                ]),
            )
            .await;
        });

        let paths = vec![String::from("trunk")];
        let out = session
            .get_mergeinfo(&paths, None, MergeInfoInheritance::Explicit, false)
            .await
            .unwrap();
        assert_eq!(out.get("trunk").map(String::as_str), Some("/trunk:1-2"));

        server_task.await.unwrap();
    });
}
