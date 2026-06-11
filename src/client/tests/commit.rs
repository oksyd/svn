use super::*;

#[test]
fn commit_builder_put_file_creates_missing_parent_dirs() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 10u64;
        let server_task = tokio::spawn(async move {
            for (path, kind) in [("a", "none"), ("a/b", "none"), ("a/b/c.txt", "none")] {
                let expected = SvnItem::List(vec![
                    SvnItem::Word("check-path".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        SvnItem::List(vec![SvnItem::Number(base_rev)]),
                    ]),
                ]);
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word(kind.to_string())]),
                    ]),
                )
                .await;
            }
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .put_file("a/b/c.txt", b"hello".to_vec());
        let commands = builder.build_editor_commands(&mut session).await.unwrap();

        assert!(matches!(
            &commands[1],
            EditorCommand::AddDir { path, .. } if path == "a"
        ));
        assert!(matches!(
            &commands[2],
            EditorCommand::AddDir { path, .. } if path == "a/b"
        ));
        assert!(matches!(
            &commands[3],
            EditorCommand::AddFile { path, .. } if path == "a/b/c.txt"
        ));
        assert!(matches!(commands.last(), Some(EditorCommand::CloseEdit)));

        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_preserves_space_only_directory_names() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 11u64;
        let server_task = tokio::spawn(async move {
            for (path, kind) in [(" ", "none"), (" /file.txt", "none")] {
                let expected = SvnItem::List(vec![
                    SvnItem::Word("check-path".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        SvnItem::List(vec![SvnItem::Number(base_rev)]),
                    ]),
                ]);
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word(kind.to_string())]),
                    ]),
                )
                .await;
            }
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .put_file(" /file.txt", b"hello".to_vec());
        let commands = builder.build_editor_commands(&mut session).await.unwrap();

        assert!(matches!(
            &commands[1],
            EditorCommand::AddDir { path, .. } if path == " "
        ));
        assert!(commands.iter().any(|command| matches!(
            command,
            EditorCommand::AddFile { path, .. } if path == " /file.txt"
        )));

        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_delete_emits_delete_entry() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 5u64;
        let server_task = tokio::spawn(async move {
            for (path, kind) in [("trunk", "dir"), ("trunk/old.txt", "file")] {
                let expected = SvnItem::List(vec![
                    SvnItem::Word("check-path".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        SvnItem::List(vec![SvnItem::Number(base_rev)]),
                    ]),
                ]);
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word(kind.to_string())]),
                    ]),
                )
                .await;
            }
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .delete("trunk/old.txt");
        let commands = builder.build_editor_commands(&mut session).await.unwrap();

        assert!(commands.iter().any(|c| matches!(
            c,
            EditorCommand::DeleteEntry { path, rev, .. } if path == "trunk/old.txt" && *rev == base_rev
        )));

        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_file_prop_emits_change_file_prop() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 7u64;
        let server_task = tokio::spawn(async move {
            for (path, kind) in [("trunk", "dir"), ("trunk/hello.txt", "file")] {
                let expected = SvnItem::List(vec![
                    SvnItem::Word("check-path".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        SvnItem::List(vec![SvnItem::Number(base_rev)]),
                    ]),
                ]);
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word(kind.to_string())]),
                    ]),
                )
                .await;
            }
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .set_file_prop("trunk/hello.txt", "svn:mime-type", b"text/plain".to_vec());
        let commands = builder.build_editor_commands(&mut session).await.unwrap();

        assert!(commands.iter().any(|c| matches!(
            c,
            EditorCommand::ChangeFileProp { name, value, .. }
                if name == "svn:mime-type" && value.as_deref() == Some(b"text/plain".as_slice())
        )));

        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_add_file_rejects_existing_file() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 8u64;
        let server_task = tokio::spawn(async move {
            for (path, kind) in [("trunk", "dir"), ("trunk/new.txt", "file")] {
                let expected = SvnItem::List(vec![
                    SvnItem::Word("check-path".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        SvnItem::List(vec![SvnItem::Number(base_rev)]),
                    ]),
                ]);
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word(kind.to_string())]),
                    ]),
                )
                .await;
            }
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .add_file("trunk/new.txt", b"hello".to_vec());
        let err = builder
            .build_editor_commands(&mut session)
            .await
            .unwrap_err();

        assert!(
            matches!(err, SvnError::Protocol(message) if message == "add-file target already exists at trunk/new.txt")
        );
        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_replace_file_rejects_missing_file() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 9u64;
        let server_task = tokio::spawn(async move {
            for (path, kind) in [("trunk", "dir"), ("trunk/missing.txt", "none")] {
                let expected = SvnItem::List(vec![
                    SvnItem::Word("check-path".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        SvnItem::List(vec![SvnItem::Number(base_rev)]),
                    ]),
                ]);
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word(kind.to_string())]),
                    ]),
                )
                .await;
            }
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .replace_file("trunk/missing.txt", b"hello".to_vec());
        let err = builder
            .build_editor_commands(&mut session)
            .await
            .unwrap_err();

        assert!(
            matches!(err, SvnError::Protocol(message) if message == "replace-file target does not exist at trunk/missing.txt")
        );
        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_add_dir_rejects_existing_dir() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 12u64;
        let server_task = tokio::spawn(async move {
            for (path, kind) in [("trunk", "dir"), ("trunk/newdir", "dir")] {
                let expected = SvnItem::List(vec![
                    SvnItem::Word("check-path".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        SvnItem::List(vec![SvnItem::Number(base_rev)]),
                    ]),
                ]);
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word(kind.to_string())]),
                    ]),
                )
                .await;
            }
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .add_dir("trunk/newdir");
        let err = builder
            .build_editor_commands(&mut session)
            .await
            .unwrap_err();

        assert!(
            matches!(err, SvnError::Protocol(message) if message == "add-dir target already exists at trunk/newdir")
        );
        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_copy_file_emits_add_file_copy_from() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 3u64;
        let server_task = tokio::spawn(async move {
            // copy source kind lookup
            for (path, kind) in [
                ("trunk/a.txt", "file"),
                ("branches", "none"),
                ("branches/b.txt", "none"),
            ] {
                let expected = SvnItem::List(vec![
                    SvnItem::Word("check-path".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        SvnItem::List(vec![SvnItem::Number(base_rev)]),
                    ]),
                ]);
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word(kind.to_string())]),
                    ]),
                )
                .await;
            }
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .copy("trunk/a.txt", "branches/b.txt");
        let commands = builder.build_editor_commands(&mut session).await.unwrap();

        assert!(commands.iter().any(|c| matches!(
            c,
            EditorCommand::AddFile { path, copy_from, .. }
                if path == "branches/b.txt"
                    && matches!(
                        copy_from.as_ref(),
                        Some((p, r)) if p == "svn://example.com:3690/repo/trunk/a.txt" && *r == base_rev
                    )
        )));

        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_copy_dir_emits_add_dir_copy_from() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 4u64;
        let server_task = tokio::spawn(async move {
            for (path, kind) in [
                ("trunk/srcdir", "dir"),
                ("branches", "none"),
                ("branches/copied", "none"),
            ] {
                let expected = SvnItem::List(vec![
                    SvnItem::Word("check-path".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(path.as_bytes().to_vec()),
                        SvnItem::List(vec![SvnItem::Number(base_rev)]),
                    ]),
                ]);
                assert_eq!(read_line(&mut server).await, encode_line(&expected));
                write_item_line(&mut server, &auth_request("realm")).await;
                write_item_line(
                    &mut server,
                    &SvnItem::List(vec![
                        SvnItem::Word("success".to_string()),
                        SvnItem::List(vec![SvnItem::Word(kind.to_string())]),
                    ]),
                )
                .await;
            }
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .copy_dir("trunk/srcdir", "branches/copied");
        let commands = builder.build_editor_commands(&mut session).await.unwrap();

        assert!(commands.iter().any(|c| matches!(
            c,
            EditorCommand::AddDir { path, copy_from, .. }
                if path == "branches/copied"
                    && matches!(
                        copy_from.as_ref(),
                        Some((p, r)) if p == "svn://example.com:3690/repo/trunk/srcdir" && *r == base_rev
                    )
        )));

        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_copy_file_rejects_dir_source() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 4u64;
        let server_task = tokio::spawn(async move {
            let expected = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/srcdir".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(base_rev)]),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected));
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::Word("dir".to_string())]),
                ]),
            )
            .await;
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .copy_file("trunk/srcdir", "branches/copied");
        let err = builder
            .build_editor_commands(&mut session)
            .await
            .unwrap_err();

        assert!(
            matches!(err, SvnError::Protocol(message) if message == "copy-file source is not a file at trunk/srcdir@4")
        );
        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_copy_dir_rejects_file_source() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 4u64;
        let server_task = tokio::spawn(async move {
            let expected = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/a.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(base_rev)]),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected));
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

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .copy_dir("trunk/a.txt", "branches/copied");
        let err = builder
            .build_editor_commands(&mut session)
            .await
            .unwrap_err();

        assert!(
            matches!(err, SvnError::Protocol(message) if message == "copy-dir source is not a directory at trunk/a.txt@4")
        );
        server_task.await.unwrap();
    });
}

#[test]
fn commit_builder_rejects_edit_inside_copied_directory() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 3u64;
        let server_task = tokio::spawn(async move {
            let expected = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/srcdir".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(base_rev)]),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected));
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::Word("dir".to_string())]),
                ]),
            )
            .await;
        });

        let builder = crate::CommitBuilder::new()
            .with_base_rev(base_rev)
            .copy("trunk/srcdir", "branches/copied")
            .put_file("branches/copied/file.txt", b"hello".to_vec());
        let err = builder
            .build_editor_commands(&mut session)
            .await
            .unwrap_err();

        assert!(
            matches!(err, SvnError::Protocol(message) if message.contains("inside copied directory"))
        );
        server_task.await.unwrap();
    });
}

#[test]
fn commit_stream_builder_sends_svndiff_fulltext_from_reader() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 1u64;
        let contents = b"hello".to_vec();
        let expected_svndiff =
            encode_fulltext_with_options(SvndiffVersion::V0, &contents, 0, 64 * 1024).unwrap();

        let server_task = tokio::spawn(async move {
            // check-path hello.txt -> none
            let expected_check = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"hello.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(base_rev)]),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected_check));
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::Word("none".to_string())]),
                ]),
            )
            .await;

            // commit request
            let mut rev_props = PropertyList::new();
            rev_props.insert("svn:log".to_string(), b"msg".to_vec());
            let expected_commit = SvnItem::List(vec![
                SvnItem::Word("commit".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"msg".to_vec()),
                    SvnItem::List(Vec::new()),
                    SvnItem::Bool(false),
                    encode_proplist(&rev_props),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected_commit));
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            fn encode_cmd(cmd: &EditorCommand) -> Vec<u8> {
                let mut buf = Vec::new();
                encode_editor_command(cmd, &mut buf).unwrap();
                buf
            }

            // editor drive
            assert_eq!(
                read_line(&mut server).await,
                encode_cmd(&EditorCommand::OpenRoot {
                    rev: Some(base_rev),
                    token: "r".to_string(),
                })
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_cmd(&EditorCommand::AddFile {
                    path: "hello.txt".to_string(),
                    dir_token: "r".to_string(),
                    file_token: "f1".to_string(),
                    copy_from: None,
                })
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_cmd(&EditorCommand::ApplyTextDelta {
                    file_token: "f1".to_string(),
                    base_checksum: None,
                })
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_cmd(&EditorCommand::TextDeltaChunk {
                    file_token: "f1".to_string(),
                    chunk: expected_svndiff,
                })
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_cmd(&EditorCommand::TextDeltaEnd {
                    file_token: "f1".to_string(),
                })
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_cmd(&EditorCommand::CloseFile {
                    file_token: "f1".to_string(),
                    text_checksum: None,
                })
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_cmd(&EditorCommand::CloseDir {
                    dir_token: "r".to_string(),
                })
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_cmd(&EditorCommand::CloseEdit)
            );

            // commit response
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            write_item_line(&mut server, &auth_request("realm-3")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![SvnItem::Number(base_rev + 1)]),
            )
            .await;
        });

        let builder = crate::CommitStreamBuilder::new()
            .with_base_rev(base_rev)
            .with_zlib_level(0)
            .put_file_reader("hello.txt", std::io::Cursor::new(contents));

        let info = builder
            .commit(&mut session, &CommitOptions::new("msg"))
            .await
            .unwrap();
        assert_eq!(info.new_rev, base_rev + 1);

        server_task.await.unwrap();
    });
}

#[test]
fn commit_stream_builder_rejects_duplicate_paths_before_io() {
    run_async(async {
        let client = RaSvnClient::new(SvnUrl::parse("svn://example.com/repo").unwrap(), None, None);
        let mut session = RaSvnSession {
            client,
            conn: None,
            server_info: None,
            allow_reconnect: false,
        };

        let builder = crate::CommitStreamBuilder::new()
            .with_base_rev(1)
            .put_file_reader("trunk/hello.txt", std::io::Cursor::new(b"one".to_vec()))
            .put_file_reader("./trunk/hello.txt", std::io::Cursor::new(b"two".to_vec()));
        let err = builder
            .commit(&mut session, &CommitOptions::new("msg"))
            .await
            .unwrap_err();

        assert!(matches!(err, SvnError::Protocol(message) if message.contains("multiple readers")));
    });
}

#[test]
fn commit_stream_builder_add_file_rejects_existing_file_before_commit() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 4u64;
        let server_task = tokio::spawn(async move {
            let expected_check = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/existing.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(base_rev)]),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected_check));
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

        let builder = crate::CommitStreamBuilder::new()
            .with_base_rev(base_rev)
            .add_file_reader(
                "trunk/existing.txt",
                std::io::Cursor::new(b"hello".to_vec()),
            );
        let err = builder
            .commit(&mut session, &CommitOptions::new("msg"))
            .await
            .unwrap_err();

        assert!(
            matches!(err, SvnError::Protocol(message) if message == "add-file target already exists at trunk/existing.txt")
        );
        server_task.await.unwrap();
    });
}

#[test]
fn commit_stream_builder_replace_file_rejects_missing_file_before_commit() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let base_rev = 4u64;
        let server_task = tokio::spawn(async move {
            let expected_check = SvnItem::List(vec![
                SvnItem::Word("check-path".to_string()),
                SvnItem::List(vec![
                    SvnItem::String(b"trunk/missing.txt".to_vec()),
                    SvnItem::List(vec![SvnItem::Number(base_rev)]),
                ]),
            ]);
            assert_eq!(read_line(&mut server).await, encode_line(&expected_check));
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::Word("none".to_string())]),
                ]),
            )
            .await;
        });

        let builder = crate::CommitStreamBuilder::new()
            .with_base_rev(base_rev)
            .replace_file_reader("trunk/missing.txt", std::io::Cursor::new(b"hello".to_vec()));
        let err = builder
            .commit(&mut session, &CommitOptions::new("msg"))
            .await
            .unwrap_err();

        assert!(
            matches!(err, SvnError::Protocol(message) if message == "replace-file target does not exist at trunk/missing.txt")
        );
        server_task.await.unwrap();
    });
}

#[test]
fn commit_sends_editor_commands_and_parses_commit_info() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_commit = SvnItem::List(vec![
            SvnItem::Word("commit".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"msg".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::Bool(false),
                SvnItem::List(vec![SvnItem::List(vec![
                    SvnItem::String(b"svn:log".to_vec()),
                    SvnItem::String(b"msg".to_vec()),
                ])]),
            ]),
        ]);

        let expected_open_root = SvnItem::List(vec![
            SvnItem::Word("open-root".to_string()),
            SvnItem::List(vec![
                SvnItem::List(vec![SvnItem::Number(1)]),
                SvnItem::String(b"root".to_vec()),
            ]),
        ]);

        let expected_close_edit = SvnItem::List(vec![
            SvnItem::Word("close-edit".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let commit_info = SvnItem::List(vec![
            SvnItem::Number(5),
            SvnItem::List(vec![SvnItem::String(b"2025-01-01".to_vec())]),
            SvnItem::List(vec![SvnItem::String(b"alice".to_vec())]),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_commit));
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(&mut server, &cmd_success).await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_open_root)
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_close_edit)
            );
            write_item_line(&mut server, &cmd_success).await;
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(&mut server, &commit_info).await;
        });

        let info = session
            .commit(
                &CommitOptions::new("msg"),
                &[
                    EditorCommand::OpenRoot {
                        rev: Some(1),
                        token: "root".to_string(),
                    },
                    EditorCommand::CloseEdit,
                ],
            )
            .await
            .unwrap();

        server_task.await.unwrap();
        assert_eq!(info.new_rev, 5);
        assert_eq!(info.date.as_deref(), Some("2025-01-01"));
        assert_eq!(info.author.as_deref(), Some("alice"));
        assert_eq!(info.post_commit_err.as_deref(), None);
    });
}
