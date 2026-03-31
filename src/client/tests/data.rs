use super::*;

#[test]
fn get_file_with_iprops_uses_get_iprops_command() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;
        session.server_info = Some(ServerInfo {
            server_caps: vec!["inherited-props".to_string()],
            repository: crate::RepositoryInfo {
                uuid: "uuid".to_string(),
                root_url: "svn://example.com/repo".to_string(),
                capabilities: Vec::new(),
            },
        });

        let expected_get_file = SvnItem::List(vec![
            SvnItem::Word("get-file".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
                SvnItem::Bool(false), // want-props
                SvnItem::Bool(true),  // want-contents
                SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
            ]),
        ]);

        let expected_get_iprops = SvnItem::List(vec![
            SvnItem::Word("get-iprops".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
            ]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_file)
            );
            write_item_line(&mut server, &auth_request("realm-1")).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![
                        SvnItem::List(Vec::new()),
                        SvnItem::Number(5),
                        SvnItem::List(Vec::new()),
                    ]),
                ]),
            )
            .await;
            write_item_line(&mut server, &SvnItem::String(b"data".to_vec())).await;
            write_item_line(&mut server, &SvnItem::String(Vec::new())).await;
            write_item_line(&mut server, &cmd_success).await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_iprops)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::List(vec![SvnItem::List(vec![
                        SvnItem::String(b"/".to_vec()),
                        SvnItem::List(vec![SvnItem::List(vec![
                            SvnItem::String(b"p".to_vec()),
                            SvnItem::String(b"v".to_vec()),
                        ])]),
                    ])])]),
                ]),
            )
            .await;
        });

        let mut out = tokio::io::sink();
        let options = GetFileOptions {
            rev: 5,
            want_props: false,
            want_iprops: true,
            max_bytes: 1024,
        };
        let result = session
            .get_file_with_options("trunk/file.txt", &options, &mut out)
            .await
            .unwrap();

        assert_eq!(result.bytes_written, 4);
        assert_eq!(result.inherited_props.len(), 1);
        assert_eq!(result.inherited_props[0].path, "/");
        assert_eq!(result.inherited_props[0].props.get("p").unwrap(), b"v");

        server_task.await.unwrap();
    });
}

#[test]
fn get_locations_reads_entries_until_done() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_get_locations = SvnItem::List(vec![
            SvnItem::Word("get-locations".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::Number(9),
                SvnItem::List(vec![SvnItem::Number(1), SvnItem::Number(2)]),
            ]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_locations)
            );
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Number(1),
                    SvnItem::String(b"/trunk/file.txt".to_vec()),
                ]),
            )
            .await;
            write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let revs = [1u64, 2u64];
        let entries = session
            .get_locations("trunk/file.txt", 9, &revs)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].rev, 1);
        assert_eq!(entries[0].path, "trunk/file.txt");

        server_task.await.unwrap();
    });
}

#[test]
fn get_location_segments_reads_entries_until_done() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected = SvnItem::List(vec![
            SvnItem::Word("get-location-segments".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::List(vec![SvnItem::Number(9)]),
                SvnItem::List(Vec::new()),
                SvnItem::List(Vec::new()),
            ]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected));
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Number(1),
                    SvnItem::Number(2),
                    SvnItem::String(b"/trunk/file.txt".to_vec()),
                ]),
            )
            .await;
            write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let segments = session
            .get_location_segments("trunk/file.txt", 9, None, None)
            .await
            .unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].range_start, 1);
        assert_eq!(segments[0].range_end, 2);
        assert_eq!(segments[0].path.as_deref(), Some("/trunk/file.txt"));

        server_task.await.unwrap();
    });
}

#[test]
fn get_file_revs_reads_entries_and_delta_chunks() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_get_file_revs = SvnItem::List(vec![
            SvnItem::Word("get-file-revs".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::List(vec![SvnItem::Number(1)]),
                SvnItem::List(vec![SvnItem::Number(2)]),
                SvnItem::Bool(false),
            ]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_file_revs)
            );
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::String(b"/trunk/file.txt".to_vec()),
                    SvnItem::Number(2),
                    SvnItem::List(vec![SvnItem::List(vec![
                        SvnItem::String(b"svn:author".to_vec()),
                        SvnItem::String(b"alice".to_vec()),
                    ])]),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            write_item_line(&mut server, &SvnItem::String(b"delta".to_vec())).await;
            write_item_line(&mut server, &SvnItem::String(Vec::new())).await;
            write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let revs = session
            .get_file_revs("trunk/file.txt", Some(1), Some(2), false)
            .await
            .unwrap();
        assert_eq!(revs.len(), 1);
        assert_eq!(revs[0].path, "trunk/file.txt");
        assert_eq!(revs[0].rev, 2);
        assert_eq!(revs[0].rev_props.get("svn:author").unwrap(), b"alice");
        assert_eq!(revs[0].delta_chunks, vec![b"delta".to_vec()]);

        server_task.await.unwrap();
    });
}

#[test]
fn get_file_revs_with_contents_applies_deltas_and_reuses_contents_on_empty_delta() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let delta1 =
            encode_fulltext_with_options(SvndiffVersion::V0, b"hello\n", 0, 64 * 1024).unwrap();
        let delta3 =
            encode_fulltext_with_options(SvndiffVersion::V0, b"world\n", 0, 64 * 1024).unwrap();

        let expected_get_file_revs = SvnItem::List(vec![
            SvnItem::Word("get-file-revs".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::List(vec![SvnItem::Number(1)]),
                SvnItem::List(vec![SvnItem::Number(3)]),
                SvnItem::Bool(false),
            ]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_file_revs)
            );
            write_item_line(&mut server, &auth_request("realm")).await;

            // rev 1: fulltext delta ("hello\n")
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::String(b"/trunk/file.txt".to_vec()),
                    SvnItem::Number(1),
                    SvnItem::List(Vec::new()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            write_item_line(&mut server, &SvnItem::String(delta1)).await;
            write_item_line(&mut server, &SvnItem::String(Vec::new())).await;

            // rev 2: props-only change (no delta)
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::String(b"/trunk/file.txt".to_vec()),
                    SvnItem::Number(2),
                    SvnItem::List(Vec::new()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            write_item_line(&mut server, &SvnItem::String(Vec::new())).await;

            // rev 3: fulltext delta ("world\n")
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::String(b"/trunk/file.txt".to_vec()),
                    SvnItem::Number(3),
                    SvnItem::List(Vec::new()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            write_item_line(&mut server, &SvnItem::String(delta3)).await;
            write_item_line(&mut server, &SvnItem::String(Vec::new())).await;

            write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let revs = session
            .get_file_revs_with_contents("trunk/file.txt", Some(1), Some(3), false, 1024)
            .await
            .unwrap();
        assert_eq!(revs.len(), 3);
        assert_eq!(revs[0].file_rev.path, "trunk/file.txt");
        assert_eq!(revs[0].file_rev.rev, 1);
        assert_eq!(revs[0].contents, b"hello\n");
        assert_eq!(revs[1].file_rev.rev, 2);
        assert_eq!(revs[1].contents, b"hello\n");
        assert_eq!(revs[2].file_rev.rev, 3);
        assert_eq!(revs[2].contents, b"world\n");

        server_task.await.unwrap();
    });
}

#[test]
fn get_deleted_rev_returns_none_on_missing_revision_failure() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected = SvnItem::List(vec![
            SvnItem::Word("get-deleted-rev".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::Number(5),
                SvnItem::Number(7),
            ]),
        ]);

        let cmd_failure = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(123),
                SvnItem::String(b"missing revision".to_vec()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(vec![SvnItem::Number(9)]),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected));
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(&mut server, &cmd_failure).await;

            assert_eq!(read_line(&mut server).await, encode_line(&expected));
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let deleted = session
            .get_deleted_rev("trunk/file.txt", 5, 7)
            .await
            .unwrap();
        assert_eq!(deleted, None);

        let deleted = session
            .get_deleted_rev("trunk/file.txt", 5, 7)
            .await
            .unwrap();
        assert_eq!(deleted, Some(9));

        server_task.await.unwrap();
    });
}

#[test]
fn proplist_file_uses_get_file_without_extra_params() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_check_path = SvnItem::List(vec![
            SvnItem::Word("check-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
            ]),
        ]);

        let expected_get_file = SvnItem::List(vec![
            SvnItem::Word("get-file".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
                SvnItem::Bool(true),  // want-props
                SvnItem::Bool(false), // want-contents
                SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
            ]),
        ]);

        let response_file_props = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(vec![
                SvnItem::List(Vec::new()),
                SvnItem::Number(5),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::String(b"p".to_vec()),
                    SvnItem::String(b"v".to_vec()),
                ])]),
            ]),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_check_path)
            );
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::Word("file".to_string())]),
                ]),
            )
            .await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_file)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(&mut server, &response_file_props).await;
        });

        let props = session
            .proplist("trunk/file.txt", Some(5))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(props.get("p").unwrap().as_slice(), b"v");

        server_task.await.unwrap();
    });
}

#[test]
fn proplist_dir_sends_dirent_fields_placeholder() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_check_path = SvnItem::List(vec![
            SvnItem::Word("check-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
            ]),
        ]);

        let expected_get_dir = SvnItem::List(vec![
            SvnItem::Word("get-dir".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
                SvnItem::Bool(true),  // want-props
                SvnItem::Bool(false), // want-contents
                SvnItem::List(Vec::new()),
                SvnItem::Bool(false), // want-iprops (always false; use get-iprops)
            ]),
        ]);

        let response_dir_props = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(vec![
                SvnItem::Number(5),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::String(b"p".to_vec()),
                    SvnItem::String(b"v".to_vec()),
                ])]),
                SvnItem::List(Vec::new()),
            ]),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_check_path)
            );
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::Word("dir".to_string())]),
                ]),
            )
            .await;

            assert_eq!(read_line(&mut server).await, encode_line(&expected_get_dir));
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(&mut server, &response_dir_props).await;
        });

        let props = session.proplist("trunk", Some(5)).await.unwrap().unwrap();
        assert_eq!(props.get("p").unwrap().as_slice(), b"v");

        server_task.await.unwrap();
    });
}
