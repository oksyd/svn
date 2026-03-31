use super::*;

#[test]
fn get_lock_and_get_locks_parse_lockdesc() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_get_lock = SvnItem::List(vec![
            SvnItem::Word("get-lock".to_string()),
            SvnItem::List(vec![SvnItem::String(b"trunk/file.txt".to_vec())]),
        ]);

        let expected_get_locks = SvnItem::List(vec![
            SvnItem::Word("get-locks".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk".to_vec()),
                SvnItem::List(vec![SvnItem::Word("infinity".to_string())]),
            ]),
        ]);

        let lockdesc = SvnItem::List(vec![
            SvnItem::String(b"/trunk/file.txt".to_vec()),
            SvnItem::String(b"token".to_vec()),
            SvnItem::String(b"alice".to_vec()),
            SvnItem::List(Vec::new()),
            SvnItem::String(b"2025-01-01".to_vec()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_lock)
            );
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::List(vec![lockdesc.clone()])]),
                ]),
            )
            .await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_get_locks)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::List(vec![lockdesc])]),
                ]),
            )
            .await;
        });

        let lock = session.get_lock("trunk/file.txt").await.unwrap().unwrap();
        assert_eq!(lock.path, "trunk/file.txt");
        assert_eq!(lock.owner, "alice");
        assert_eq!(lock.token, "token");

        let locks = session.get_locks("trunk", Depth::Infinity).await.unwrap();
        assert_eq!(locks.len(), 1);
        assert_eq!(locks[0].path, "trunk/file.txt");

        server_task.await.unwrap();
    });
}

#[test]
fn lock_does_not_drop_connection_on_server_failure() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_lock = SvnItem::List(vec![
            SvnItem::Word("lock".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/a.txt".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::Bool(false),
                SvnItem::List(Vec::new()),
            ]),
        ]);

        let cmd_failure = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(123),
                SvnItem::String(b"lock denied".to_vec()),
                SvnItem::String(b"file".to_vec()),
                SvnItem::Number(1),
            ])]),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_lock));
            write_item_line(&mut server, &auth_request("realm")).await;
            write_item_line(&mut server, &cmd_failure).await;
        });

        let err = session
            .lock("trunk/a.txt", &LockOptions::new())
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::Server(_)));
        assert!(session.conn.is_some());

        server_task.await.unwrap();
    });
}

#[test]
fn lock_and_unlock_round_trip() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_lock = SvnItem::List(vec![
            SvnItem::Word("lock".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/a.txt".to_vec()),
                SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
                SvnItem::Bool(false),
                SvnItem::List(vec![SvnItem::Number(12)]),
            ]),
        ]);

        let lockdesc = SvnItem::List(vec![
            SvnItem::String(b"/trunk/a.txt".to_vec()),
            SvnItem::String(b"t0".to_vec()),
            SvnItem::String(b"alice".to_vec()),
            SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
            SvnItem::String(b"2025-01-01".to_vec()),
            SvnItem::List(Vec::new()),
        ]);

        let expected_unlock = SvnItem::List(vec![
            SvnItem::Word("unlock".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/a.txt".to_vec()),
                SvnItem::List(vec![SvnItem::String(b"t0".to_vec())]),
                SvnItem::Bool(false),
            ]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_lock));
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![lockdesc]),
                ]),
            )
            .await;

            assert_eq!(read_line(&mut server).await, encode_line(&expected_unlock));
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let lock = session
            .lock(
                "trunk/a.txt",
                &LockOptions::new().with_comment("hi").with_current_rev(12),
            )
            .await
            .unwrap();
        assert_eq!(lock.path, "trunk/a.txt");
        assert_eq!(lock.token, "t0");
        assert_eq!(lock.owner, "alice");
        assert_eq!(lock.comment.as_deref(), Some("hi"));

        session
            .unlock("trunk/a.txt", &UnlockOptions::new().with_token("t0"))
            .await
            .unwrap();

        server_task.await.unwrap();
    });
}

#[test]
fn lock_many_and_unlock_many_stream_results() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_lock_many = SvnItem::List(vec![
            SvnItem::Word("lock-many".to_string()),
            SvnItem::List(vec![
                SvnItem::List(Vec::new()),
                SvnItem::Bool(true),
                SvnItem::List(vec![
                    SvnItem::List(vec![
                        SvnItem::String(b"trunk/a.txt".to_vec()),
                        SvnItem::List(vec![SvnItem::Number(1)]),
                    ]),
                    SvnItem::List(vec![
                        SvnItem::String(b"trunk/b.txt".to_vec()),
                        SvnItem::List(Vec::new()),
                    ]),
                ]),
            ]),
        ]);

        let lock_a = SvnItem::List(vec![
            SvnItem::String(b"trunk/a.txt".to_vec()),
            SvnItem::String(b"t1".to_vec()),
            SvnItem::String(b"alice".to_vec()),
            SvnItem::List(Vec::new()),
            SvnItem::String(b"2025-01-01".to_vec()),
        ]);

        let err = SvnItem::List(vec![
            SvnItem::Number(123),
            SvnItem::String(b"lock denied".to_vec()),
            SvnItem::String(b"file".to_vec()),
            SvnItem::Number(1),
        ]);

        let expected_unlock_many = SvnItem::List(vec![
            SvnItem::Word("unlock-many".to_string()),
            SvnItem::List(vec![
                SvnItem::Bool(false),
                SvnItem::List(vec![
                    SvnItem::List(vec![
                        SvnItem::String(b"trunk/a.txt".to_vec()),
                        SvnItem::List(vec![SvnItem::String(b"t1".to_vec())]),
                    ]),
                    SvnItem::List(vec![
                        SvnItem::String(b"trunk/b.txt".to_vec()),
                        SvnItem::List(Vec::new()),
                    ]),
                ]),
            ]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_lock_many)
            );
            write_item_line(&mut server, &auth_request("realm-1")).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![lock_a]),
                ]),
            )
            .await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("failure".to_string()),
                    SvnItem::List(vec![err.clone()]),
                ]),
            )
            .await;
            write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
            write_item_line(&mut server, &cmd_success).await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_unlock_many)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![SvnItem::String(b"/trunk/a.txt".to_vec())]),
                ]),
            )
            .await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("failure".to_string()),
                    SvnItem::List(vec![err]),
                ]),
            )
            .await;
            write_item_line(&mut server, &SvnItem::Word("done".to_string())).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let lock_results = session
            .lock_many(
                &LockManyOptions::new().steal_lock(),
                &[
                    LockTarget::new("trunk/a.txt").with_current_rev(1),
                    LockTarget::new("trunk/b.txt"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(lock_results.len(), 2);
        assert_eq!(lock_results[0].as_ref().unwrap().token, "t1");
        assert!(matches!(lock_results[1], Err(SvnError::Server(_))));

        let unlock_results = session
            .unlock_many(
                &UnlockManyOptions::new(),
                &[
                    UnlockTarget::new("trunk/a.txt").with_token("t1"),
                    UnlockTarget::new("trunk/b.txt"),
                ],
            )
            .await
            .unwrap();
        assert_eq!(unlock_results.len(), 2);
        assert_eq!(unlock_results[0].as_ref().unwrap(), "trunk/a.txt");
        assert!(matches!(unlock_results[1], Err(SvnError::Server(_))));

        server_task.await.unwrap();
    });
}

#[test]
fn lock_many_falls_back_to_lock_when_unsupported() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_lock_many = SvnItem::List(vec![
            SvnItem::Word("lock-many".to_string()),
            SvnItem::List(vec![
                SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
                SvnItem::Bool(true),
                SvnItem::List(vec![
                    SvnItem::List(vec![
                        SvnItem::String(b"trunk/a.txt".to_vec()),
                        SvnItem::List(vec![SvnItem::Number(1)]),
                    ]),
                    SvnItem::List(vec![
                        SvnItem::String(b"trunk/b.txt".to_vec()),
                        SvnItem::List(Vec::new()),
                    ]),
                ]),
            ]),
        ]);

        let expected_lock_a = SvnItem::List(vec![
            SvnItem::Word("lock".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/a.txt".to_vec()),
                SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
                SvnItem::Bool(true),
                SvnItem::List(vec![SvnItem::Number(1)]),
            ]),
        ]);

        let expected_lock_b = SvnItem::List(vec![
            SvnItem::Word("lock".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/b.txt".to_vec()),
                SvnItem::List(vec![SvnItem::String(b"hi".to_vec())]),
                SvnItem::Bool(true),
                SvnItem::List(Vec::new()),
            ]),
        ]);

        let unknown_cmd_err = SvnItem::List(vec![
            SvnItem::Number(999),
            SvnItem::String(b"Unknown command".to_vec()),
            SvnItem::String(b"file".to_vec()),
            SvnItem::Number(1),
        ]);

        let lock_a = SvnItem::List(vec![
            SvnItem::String(b"trunk/a.txt".to_vec()),
            SvnItem::String(b"t1".to_vec()),
            SvnItem::String(b"alice".to_vec()),
            SvnItem::List(Vec::new()),
            SvnItem::String(b"2025-01-01".to_vec()),
        ]);
        let lock_b = SvnItem::List(vec![
            SvnItem::String(b"trunk/b.txt".to_vec()),
            SvnItem::String(b"t2".to_vec()),
            SvnItem::String(b"alice".to_vec()),
            SvnItem::List(Vec::new()),
            SvnItem::String(b"2025-01-01".to_vec()),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_lock_many)
            );
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("failure".to_string()),
                    SvnItem::List(vec![unknown_cmd_err]),
                ]),
            )
            .await;

            assert_eq!(read_line(&mut server).await, encode_line(&expected_lock_a));
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![lock_a]),
                ]),
            )
            .await;

            assert_eq!(read_line(&mut server).await, encode_line(&expected_lock_b));
            write_item_line(&mut server, &auth_request("realm-3")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("success".to_string()),
                    SvnItem::List(vec![lock_b]),
                ]),
            )
            .await;

            write_item_line(&mut server, &cmd_success).await;
        });

        let lock_results = session
            .lock_many(
                &LockManyOptions::new().with_comment("hi").steal_lock(),
                &[
                    LockTarget::new("trunk/a.txt").with_current_rev(1),
                    LockTarget::new("trunk/b.txt"),
                ],
            )
            .await
            .unwrap();

        assert_eq!(lock_results.len(), 2);
        assert_eq!(lock_results[0].as_ref().unwrap().token, "t1");
        assert_eq!(lock_results[1].as_ref().unwrap().token, "t2");

        server_task.await.unwrap();
    });
}

#[test]
fn unlock_many_falls_back_to_unlock_when_unsupported() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        let expected_unlock_many = SvnItem::List(vec![
            SvnItem::Word("unlock-many".to_string()),
            SvnItem::List(vec![
                SvnItem::Bool(true),
                SvnItem::List(vec![
                    SvnItem::List(vec![
                        SvnItem::String(b"trunk/a.txt".to_vec()),
                        SvnItem::List(vec![SvnItem::String(b"t1".to_vec())]),
                    ]),
                    SvnItem::List(vec![
                        SvnItem::String(b"trunk/b.txt".to_vec()),
                        SvnItem::List(Vec::new()),
                    ]),
                ]),
            ]),
        ]);

        let expected_unlock_a = SvnItem::List(vec![
            SvnItem::Word("unlock".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/a.txt".to_vec()),
                SvnItem::List(vec![SvnItem::String(b"t1".to_vec())]),
                SvnItem::Bool(true),
            ]),
        ]);

        let expected_unlock_b = SvnItem::List(vec![
            SvnItem::Word("unlock".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/b.txt".to_vec()),
                SvnItem::List(Vec::new()),
                SvnItem::Bool(true),
            ]),
        ]);

        let unknown_cmd_err = SvnItem::List(vec![
            SvnItem::Number(999),
            SvnItem::String(b"Unknown command".to_vec()),
            SvnItem::String(b"file".to_vec()),
            SvnItem::Number(1),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_unlock_many)
            );
            write_item_line(&mut server, &auth_request("realm-1")).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("failure".to_string()),
                    SvnItem::List(vec![unknown_cmd_err]),
                ]),
            )
            .await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_unlock_a)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;
            write_item_line(&mut server, &cmd_success).await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_unlock_b)
            );
            write_item_line(&mut server, &auth_request("realm-3")).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let unlock_results = session
            .unlock_many(
                &UnlockManyOptions::new().break_lock(),
                &[
                    UnlockTarget::new("trunk/a.txt").with_token("t1"),
                    UnlockTarget::new("trunk/b.txt"),
                ],
            )
            .await
            .unwrap();

        assert_eq!(unlock_results.len(), 2);
        assert_eq!(unlock_results[0].as_ref().unwrap(), "trunk/a.txt");
        assert_eq!(unlock_results[1].as_ref().unwrap(), "trunk/b.txt");

        server_task.await.unwrap();
    });
}
