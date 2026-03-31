use super::*;

#[test]
fn update_drives_report_and_editor() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl EditorEventHandler for Collector {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                self.events.push(event);
                Ok(())
            }
        }

        let report = Report {
            commands: vec![
                ReportCommand::SetPath {
                    path: "".to_string(),
                    rev: 0,
                    start_empty: true,
                    lock_token: None,
                    depth: Depth::Infinity,
                },
                ReportCommand::FinishReport,
            ],
        };

        let expected_update = SvnItem::List(vec![
            SvnItem::Word("update".to_string()),
            SvnItem::List(vec![
                SvnItem::List(Vec::new()),
                SvnItem::String(Vec::new()),
                SvnItem::Bool(true),
                SvnItem::Word("infinity".to_string()),
                SvnItem::Bool(false),
                SvnItem::Bool(false),
            ]),
        ]);
        let expected_set_path = SvnItem::List(vec![
            SvnItem::Word("set-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
                SvnItem::Bool(true),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
            ]),
        ]);
        let expected_finish_report = SvnItem::List(vec![
            SvnItem::Word("finish-report".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        let expected_cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_update));
            write_item_line(&mut server, &auth_request("realm-1")).await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_set_path)
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_finish_report)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_cmd_success)
            );
            write_item_line(&mut server, &expected_cmd_success).await;
        });

        let mut handler = Collector { events: Vec::new() };
        let options = UpdateOptions::new("", Depth::Infinity).without_copyfrom_args();
        session
            .update(&options, &report, &mut handler)
            .await
            .unwrap();

        server_task.await.unwrap();
        assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
    });
}

#[test]
fn update_returns_handler_error_even_if_server_succeeds() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        struct Failer;

        impl EditorEventHandler for Failer {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                if matches!(event, EditorEvent::CloseEdit) {
                    return Err(SvnError::Protocol("boom".into()));
                }
                Ok(())
            }
        }

        let report = Report {
            commands: vec![
                ReportCommand::SetPath {
                    path: "".to_string(),
                    rev: 0,
                    start_empty: true,
                    lock_token: None,
                    depth: Depth::Infinity,
                },
                ReportCommand::FinishReport,
            ],
        };

        let expected_update = SvnItem::List(vec![
            SvnItem::Word("update".to_string()),
            SvnItem::List(vec![
                SvnItem::List(Vec::new()),
                SvnItem::String(Vec::new()),
                SvnItem::Bool(true),
                SvnItem::Word("infinity".to_string()),
                SvnItem::Bool(false),
                SvnItem::Bool(false),
            ]),
        ]);
        let expected_set_path = SvnItem::List(vec![
            SvnItem::Word("set-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
                SvnItem::Bool(true),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
            ]),
        ]);
        let expected_finish_report = SvnItem::List(vec![
            SvnItem::Word("finish-report".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        let expected_failure = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::String(b"protocol error: boom".to_vec()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);
        let expected_cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_update));
            write_item_line(&mut server, &auth_request("realm-1")).await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_set_path)
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_finish_report)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            let failure_line = read_line(&mut server).await;
            write_item_line(&mut server, &expected_cmd_success).await;
            failure_line
        });

        let mut handler = Failer;
        let options = UpdateOptions::new("", Depth::Infinity).without_copyfrom_args();
        let err = session
            .update(&options, &report, &mut handler)
            .await
            .unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "boom"));

        let failure_line = server_task.await.unwrap();
        assert_eq!(failure_line, encode_line(&expected_failure));
    });
}

#[test]
fn switch_drives_report_and_editor() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl EditorEventHandler for Collector {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                self.events.push(event);
                Ok(())
            }
        }

        let report = Report {
            commands: vec![
                ReportCommand::SetPath {
                    path: "".to_string(),
                    rev: 0,
                    start_empty: true,
                    lock_token: None,
                    depth: Depth::Infinity,
                },
                ReportCommand::FinishReport,
            ],
        };

        let switch_url = SvnUrl::parse("svn://example.com/repo/branch").unwrap();
        let switch_url = switch_url.url;

        let expected_switch = SvnItem::List(vec![
            SvnItem::Word("switch".to_string()),
            SvnItem::List(vec![
                SvnItem::List(Vec::new()),
                SvnItem::String(Vec::new()),
                SvnItem::Bool(true),
                SvnItem::String(switch_url.as_bytes().to_vec()),
                SvnItem::Word("infinity".to_string()),
                SvnItem::Bool(false),
                SvnItem::Bool(false),
            ]),
        ]);

        let expected_set_path = SvnItem::List(vec![
            SvnItem::Word("set-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
                SvnItem::Bool(true),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
            ]),
        ]);
        let expected_finish_report = SvnItem::List(vec![
            SvnItem::Word("finish-report".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        let expected_cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_switch));
            write_item_line(&mut server, &auth_request("realm-1")).await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_set_path)
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_finish_report)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_cmd_success)
            );
            write_item_line(&mut server, &expected_cmd_success).await;
        });

        let mut handler = Collector { events: Vec::new() };
        let options = SwitchOptions::new("", switch_url, Depth::Infinity).without_copyfrom_args();
        session
            .switch(&options, &report, &mut handler)
            .await
            .unwrap();

        server_task.await.unwrap();
        assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
    });
}

#[test]
fn status_drives_report_and_editor() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl EditorEventHandler for Collector {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                self.events.push(event);
                Ok(())
            }
        }

        let report = Report {
            commands: vec![
                ReportCommand::SetPath {
                    path: "".to_string(),
                    rev: 0,
                    start_empty: true,
                    lock_token: None,
                    depth: Depth::Infinity,
                },
                ReportCommand::FinishReport,
            ],
        };

        let expected_status = SvnItem::List(vec![
            SvnItem::Word("status".to_string()),
            SvnItem::List(vec![
                SvnItem::String(Vec::new()),
                SvnItem::Bool(true),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
            ]),
        ]);

        let expected_set_path = SvnItem::List(vec![
            SvnItem::Word("set-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
                SvnItem::Bool(true),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
            ]),
        ]);
        let expected_finish_report = SvnItem::List(vec![
            SvnItem::Word("finish-report".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        let expected_cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_status));
            write_item_line(&mut server, &auth_request("realm-1")).await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_set_path)
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_finish_report)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_cmd_success)
            );
            write_item_line(&mut server, &expected_cmd_success).await;
        });

        let mut handler = Collector { events: Vec::new() };
        let options = StatusOptions::new("", Depth::Infinity);
        session
            .status(&options, &report, &mut handler)
            .await
            .unwrap();

        server_task.await.unwrap();
        assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
    });
}

#[test]
fn diff_drives_report_and_editor() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl EditorEventHandler for Collector {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                self.events.push(event);
                Ok(())
            }
        }

        let report = Report {
            commands: vec![
                ReportCommand::SetPath {
                    path: "".to_string(),
                    rev: 0,
                    start_empty: true,
                    lock_token: None,
                    depth: Depth::Infinity,
                },
                ReportCommand::FinishReport,
            ],
        };

        let versus_url = SvnUrl::parse("svn://example.com/repo/branch").unwrap().url;

        let expected_diff = SvnItem::List(vec![
            SvnItem::Word("diff".to_string()),
            SvnItem::List(vec![
                SvnItem::List(Vec::new()),
                SvnItem::String(Vec::new()),
                SvnItem::Bool(true),
                SvnItem::Bool(false),
                SvnItem::String(versus_url.as_bytes().to_vec()),
                SvnItem::Bool(true),
                SvnItem::Word("infinity".to_string()),
            ]),
        ]);

        let expected_set_path = SvnItem::List(vec![
            SvnItem::Word("set-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
                SvnItem::Bool(true),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
            ]),
        ]);
        let expected_finish_report = SvnItem::List(vec![
            SvnItem::Word("finish-report".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        let expected_cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_diff));
            write_item_line(&mut server, &auth_request("realm-1")).await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_set_path)
            );
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_finish_report)
            );
            write_item_line(&mut server, &auth_request("realm-2")).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_cmd_success)
            );
            write_item_line(&mut server, &expected_cmd_success).await;
        });

        let mut handler = Collector { events: Vec::new() };
        let options = DiffOptions::new("", versus_url, Depth::Infinity);
        session.diff(&options, &report, &mut handler).await.unwrap();

        server_task.await.unwrap();
        assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
    });
}

#[test]
fn replay_range_emits_revprops_and_finish_replay() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl EditorEventHandler for Collector {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                self.events.push(event);
                Ok(())
            }
        }

        let expected_replay_range = SvnItem::List(vec![
            SvnItem::Word("replay-range".to_string()),
            SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::Number(2),
                SvnItem::Number(0),
                SvnItem::Bool(true),
            ]),
        ]);

        let revprops_1 = SvnItem::List(vec![
            SvnItem::Word("revprops".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::String(b"svn:author".to_vec()),
                SvnItem::String(b"alice".to_vec()),
            ])]),
        ]);
        let revprops_2 = SvnItem::List(vec![
            SvnItem::Word("revprops".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::String(b"svn:author".to_vec()),
                SvnItem::String(b"bob".to_vec()),
            ])]),
        ]);
        let finish_replay = SvnItem::List(vec![
            SvnItem::Word("finish-replay".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        let server_task = tokio::spawn(async move {
            assert_eq!(
                read_line(&mut server).await,
                encode_line(&expected_replay_range)
            );
            write_item_line(&mut server, &auth_request("realm")).await;

            write_item_line(&mut server, &revprops_1).await;
            write_item_line(&mut server, &finish_replay).await;
            write_item_line(&mut server, &revprops_2).await;
            write_item_line(&mut server, &finish_replay).await;
            write_item_line(&mut server, &cmd_success).await;
        });

        let mut handler = Collector { events: Vec::new() };
        let options = ReplayRangeOptions::new(1, 2);
        session.replay_range(&options, &mut handler).await.unwrap();

        server_task.await.unwrap();
        assert_eq!(handler.events.len(), 4);
        assert!(matches!(handler.events[0], EditorEvent::RevProps { .. }));
        assert_eq!(handler.events[1], EditorEvent::FinishReplay);
        assert!(matches!(handler.events[2], EditorEvent::RevProps { .. }));
        assert_eq!(handler.events[3], EditorEvent::FinishReplay);

        let props = match &handler.events[0] {
            EditorEvent::RevProps { props } => Some(props),
            _ => None,
        }
        .unwrap();
        assert_eq!(
            props.get("svn:author").map(|v| v.as_slice()),
            Some(b"alice".as_slice())
        );

        let props = match &handler.events[2] {
            EditorEvent::RevProps { props } => Some(props),
            _ => None,
        }
        .unwrap();
        assert_eq!(
            props.get("svn:author").map(|v| v.as_slice()),
            Some(b"bob".as_slice())
        );
    });
}

#[test]
fn replay_sends_command_and_drives_editor() {
    run_async(async {
        let (mut session, mut server) = connected_session().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl EditorEventHandler for Collector {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                self.events.push(event);
                Ok(())
            }
        }

        let expected_replay = SvnItem::List(vec![
            SvnItem::Word("replay".to_string()),
            SvnItem::List(vec![
                SvnItem::Number(5),
                SvnItem::Number(0),
                SvnItem::Bool(true),
            ]),
        ]);

        let cmd_success = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        let server_task = tokio::spawn(async move {
            assert_eq!(read_line(&mut server).await, encode_line(&expected_replay));
            write_item_line(&mut server, &auth_request("realm")).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            assert_eq!(read_line(&mut server).await, encode_line(&cmd_success));
            write_item_line(&mut server, &cmd_success).await;
        });

        let mut handler = Collector { events: Vec::new() };
        let options = ReplayOptions::new(5);
        session.replay(&options, &mut handler).await.unwrap();

        server_task.await.unwrap();
        assert_eq!(handler.events, vec![EditorEvent::CloseEdit]);
    });
}
