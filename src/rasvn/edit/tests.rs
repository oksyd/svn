#![allow(clippy::unwrap_used)]

use std::pin::Pin;
use std::time::Duration;

use super::*;
use crate::Depth;
use crate::rasvn::conn::RaSvnConnectionConfig;
use crate::raw::SvnItem;
use crate::test_support::{
    encode_line, read_until_newline as read_line, run_async, write_item_line,
};
use crate::{
    AsyncEditorEventHandler, EditorCommand, EditorEvent, EditorEventHandler, Report, ReportCommand,
    SvnError,
};

async fn connected_conn() -> (super::super::conn::RaSvnConnection, tokio::net::TcpStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept_task = tokio::spawn(async move { listener.accept().await });
    let client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (server, _) = accept_task.await.unwrap().unwrap();

    let (read, write) = client.into_split();
    let conn = super::super::conn::RaSvnConnection::new(
        Box::new(read),
        Box::new(write),
        RaSvnConnectionConfig {
            username: None,
            password: None,
            #[cfg(feature = "cyrus-sasl")]
            host: "example.com".to_string(),
            #[cfg(feature = "cyrus-sasl")]
            local_addrport: None,
            #[cfg(feature = "cyrus-sasl")]
            remote_addrport: None,
            is_tunneled: false,
            url: "svn://example.com:3690/repo".to_string(),
            ra_client: "test-ra_svn".to_string(),
            read_timeout: Duration::from_secs(1),
            write_timeout: Duration::from_secs(1),
        },
    );
    (conn, server)
}

#[test]
fn send_report_writes_expected_commands() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;
        let mut report = Report::new();
        report
            .push(ReportCommand::SetPath {
                path: "trunk".to_string(),
                rev: 10,
                start_empty: true,
                lock_token: None,
                depth: Depth::Infinity,
            })
            .finish();

        send_report(&mut conn, &report).await.unwrap();

        let expected_set_path = SvnItem::List(vec![
            SvnItem::Word("set-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk".to_vec()),
                SvnItem::Number(10),
                SvnItem::Bool(true),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
            ]),
        ]);
        let expected_finish = SvnItem::List(vec![
            SvnItem::Word("finish-report".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        assert_eq!(
            read_line(&mut server).await,
            encode_line(&expected_set_path)
        );
        assert_eq!(read_line(&mut server).await, encode_line(&expected_finish));
    });
}

#[test]
fn send_report_normalizes_paths() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;
        let mut report = Report::new();
        report
            .push(ReportCommand::SetPath {
                path: "//trunk\\\\sub//./".to_string(),
                rev: 10,
                start_empty: true,
                lock_token: None,
                depth: Depth::Infinity,
            })
            .finish();

        send_report(&mut conn, &report).await.unwrap();

        let expected_set_path = SvnItem::List(vec![
            SvnItem::Word("set-path".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/sub".to_vec()),
                SvnItem::Number(10),
                SvnItem::Bool(true),
                SvnItem::List(Vec::new()),
                SvnItem::Word("infinity".to_string()),
            ]),
        ]);
        let expected_finish = SvnItem::List(vec![
            SvnItem::Word("finish-report".to_string()),
            SvnItem::List(Vec::new()),
        ]);

        assert_eq!(
            read_line(&mut server).await,
            encode_line(&expected_set_path)
        );
        assert_eq!(read_line(&mut server).await, encode_line(&expected_finish));
    });
}

#[test]
fn send_report_rejects_unsafe_paths() {
    run_async(async {
        let (mut conn, _server) = connected_conn().await;
        let mut report = Report::new();
        report
            .push(ReportCommand::SetPath {
                path: "trunk/../x".to_string(),
                rev: 1,
                start_empty: true,
                lock_token: None,
                depth: Depth::Infinity,
            })
            .finish();

        let err = send_report(&mut conn, &report).await.unwrap_err();
        assert!(matches!(err, SvnError::InvalidPath(_)));
    });
}

#[test]
fn send_report_requires_terminator() {
    run_async(async {
        let (mut conn, _server) = connected_conn().await;
        let report = Report {
            commands: vec![ReportCommand::DeletePath {
                path: "trunk/file.txt".to_string(),
            }],
        };
        let err = send_report(&mut conn, &report).await.unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
    });
}

#[test]
fn send_editor_command_encodes_revision_as_optional_tuple() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;

        let cmd = EditorCommand::DeleteEntry {
            path: "trunk/old.txt".to_string(),
            rev: 5,
            dir_token: "d".to_string(),
        };
        send_editor_command(&mut conn, &cmd).await.unwrap();

        let expected = SvnItem::List(vec![
            SvnItem::Word("delete-entry".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/old.txt".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
                SvnItem::String(b"d".to_vec()),
            ]),
        ]);
        assert_eq!(read_line(&mut server).await, encode_line(&expected));

        let cmd = EditorCommand::OpenDir {
            path: "trunk".to_string(),
            parent_token: "r".to_string(),
            child_token: "t".to_string(),
            rev: 5,
        };
        send_editor_command(&mut conn, &cmd).await.unwrap();

        let expected = SvnItem::List(vec![
            SvnItem::Word("open-dir".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk".to_vec()),
                SvnItem::String(b"r".to_vec()),
                SvnItem::String(b"t".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
            ]),
        ]);
        assert_eq!(read_line(&mut server).await, encode_line(&expected));

        let cmd = EditorCommand::OpenFile {
            path: "trunk/file.txt".to_string(),
            dir_token: "t".to_string(),
            file_token: "f".to_string(),
            rev: 5,
        };
        send_editor_command(&mut conn, &cmd).await.unwrap();

        let expected = SvnItem::List(vec![
            SvnItem::Word("open-file".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"trunk/file.txt".to_vec()),
                SvnItem::String(b"t".to_vec()),
                SvnItem::String(b"f".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
            ]),
        ]);
        assert_eq!(read_line(&mut server).await, encode_line(&expected));
    });
}

#[test]
fn drive_editor_sends_success_on_close_edit() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl EditorEventHandler for Collector {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                self.events.push(event);
                Ok(())
            }
        }

        let server_task = tokio::spawn(async move {
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("target-rev".to_string()),
                    SvnItem::List(vec![SvnItem::Number(42)]),
                ]),
            )
            .await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            read_line(&mut server).await
        });

        let mut handler = Collector { events: Vec::new() };
        let status = drive_editor(&mut conn, Some(&mut handler), false)
            .await
            .unwrap();
        assert!(matches!(status, EditorDriveStatus::Completed));

        let response_line = server_task.await.unwrap();
        let expected_response = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        assert_eq!(response_line, encode_line(&expected_response));
        assert_eq!(
            handler.events,
            vec![EditorEvent::TargetRev { rev: 42 }, EditorEvent::CloseEdit]
        );
    });
}

#[test]
fn drive_editor_normalizes_paths() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl EditorEventHandler for Collector {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                self.events.push(event);
                Ok(())
            }
        }

        let delete_entry = SvnItem::List(vec![
            SvnItem::Word("delete-entry".to_string()),
            SvnItem::List(vec![
                SvnItem::String(b"//trunk\\\\sub//./file.txt".to_vec()),
                SvnItem::List(vec![SvnItem::Number(5)]),
                SvnItem::String(b"d".to_vec()),
            ]),
        ]);

        let server_task = tokio::spawn(async move {
            write_item_line(&mut server, &delete_entry).await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            read_line(&mut server).await
        });

        let mut handler = Collector { events: Vec::new() };
        let status = drive_editor(&mut conn, Some(&mut handler), false)
            .await
            .unwrap();
        assert!(matches!(status, EditorDriveStatus::Completed));

        let response_line = server_task.await.unwrap();
        let expected_response = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        assert_eq!(response_line, encode_line(&expected_response));
        assert_eq!(
            handler.events,
            vec![
                EditorEvent::DeleteEntry {
                    path: "trunk/sub/file.txt".to_string(),
                    rev: 5,
                    dir_token: "d".to_string(),
                },
                EditorEvent::CloseEdit,
            ]
        );
    });
}

#[test]
fn drive_editor_rejects_unsafe_paths() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl EditorEventHandler for Collector {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                self.events.push(event);
                Ok(())
            }
        }

        let expected_failure = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::String(b"invalid path: unsafe path".to_vec()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);

        let server_task = tokio::spawn(async move {
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("delete-entry".to_string()),
                    SvnItem::List(vec![
                        SvnItem::String(b"trunk/../x".to_vec()),
                        SvnItem::List(vec![SvnItem::Number(1)]),
                        SvnItem::String(b"d".to_vec()),
                    ]),
                ]),
            )
            .await;

            let failure_line = read_line(&mut server).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("abort-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            (failure_line, server)
        });

        let mut handler = Collector { events: Vec::new() };
        let status = drive_editor(&mut conn, Some(&mut handler), false)
            .await
            .unwrap();
        assert!(matches!(
            status,
            EditorDriveStatus::Aborted(SvnError::InvalidPath(_))
        ));

        let (failure_line, mut server) = server_task.await.unwrap();
        assert_eq!(failure_line, encode_line(&expected_failure));
        assert!(handler.events.is_empty());

        let no_response =
            tokio::time::timeout(Duration::from_millis(50), read_line(&mut server)).await;
        assert!(no_response.is_err());
    });
}

#[test]
fn drive_editor_sends_failure_and_drains_on_handler_error() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;

        struct Failer;

        impl EditorEventHandler for Failer {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                if matches!(event, EditorEvent::TargetRev { .. }) {
                    return Err(SvnError::Protocol("boom".into()));
                }
                Ok(())
            }
        }

        let expected_failure = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::String(b"protocol error: boom".to_vec()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);

        let server_task = tokio::spawn(async move {
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("target-rev".to_string()),
                    SvnItem::List(vec![SvnItem::Number(1)]),
                ]),
            )
            .await;

            let failure_line = read_line(&mut server).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("abort-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            (failure_line, server)
        });

        let mut handler = Failer;
        let status = drive_editor(&mut conn, Some(&mut handler), false)
            .await
            .unwrap();
        assert!(matches!(
            status,
            EditorDriveStatus::Aborted(SvnError::Protocol(msg)) if msg == "boom"
        ));

        let (failure_line, mut server) = server_task.await.unwrap();
        assert_eq!(failure_line, encode_line(&expected_failure));

        let no_response =
            tokio::time::timeout(Duration::from_millis(50), read_line(&mut server)).await;
        assert!(no_response.is_err());
    });
}

#[test]
fn drive_editor_sends_failure_instead_of_success_on_close_edit_handler_error() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;

        struct Failer;

        impl EditorEventHandler for Failer {
            fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
                if matches!(event, EditorEvent::CloseEdit) {
                    return Err(SvnError::Protocol("boom".into()));
                }
                Ok(())
            }
        }

        let expected_failure = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::String(b"protocol error: boom".to_vec()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);

        let server_task = tokio::spawn(async move {
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            read_line(&mut server).await
        });

        let mut handler = Failer;
        let status = drive_editor(&mut conn, Some(&mut handler), false)
            .await
            .unwrap();
        assert!(matches!(
            status,
            EditorDriveStatus::Aborted(SvnError::Protocol(msg)) if msg == "boom"
        ));

        let response_line = server_task.await.unwrap();
        assert_eq!(response_line, encode_line(&expected_failure));
    });
}

#[test]
fn drive_editor_async_collects_events_and_sends_success() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;

        struct Collector {
            events: Vec<EditorEvent>,
        }

        impl AsyncEditorEventHandler for Collector {
            fn on_event<'a>(
                &'a mut self,
                event: EditorEvent,
            ) -> Pin<Box<dyn Future<Output = Result<(), SvnError>> + Send + 'a>> {
                Box::pin(async move {
                    self.events.push(event);
                    Ok(())
                })
            }
        }

        let server_task = tokio::spawn(async move {
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("target-rev".to_string()),
                    SvnItem::List(vec![SvnItem::Number(42)]),
                ]),
            )
            .await;
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            read_line(&mut server).await
        });

        let mut handler = Collector { events: Vec::new() };
        let status = drive_editor_async(&mut conn, Some(&mut handler), false)
            .await
            .unwrap();
        assert!(matches!(status, EditorDriveStatus::Completed));

        let response_line = server_task.await.unwrap();
        let expected_response = SvnItem::List(vec![
            SvnItem::Word("success".to_string()),
            SvnItem::List(Vec::new()),
        ]);
        assert_eq!(response_line, encode_line(&expected_response));
        assert_eq!(
            handler.events,
            vec![EditorEvent::TargetRev { rev: 42 }, EditorEvent::CloseEdit]
        );
    });
}

#[test]
fn drive_editor_async_sends_failure_and_drains_on_handler_error() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;

        struct Failer;

        impl AsyncEditorEventHandler for Failer {
            fn on_event<'a>(
                &'a mut self,
                event: EditorEvent,
            ) -> Pin<Box<dyn Future<Output = Result<(), SvnError>> + Send + 'a>> {
                Box::pin(async move {
                    if matches!(event, EditorEvent::TargetRev { .. }) {
                        return Err(SvnError::Protocol("boom".into()));
                    }
                    Ok(())
                })
            }
        }

        let expected_failure = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::String(b"protocol error: boom".to_vec()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);

        let server_task = tokio::spawn(async move {
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("target-rev".to_string()),
                    SvnItem::List(vec![SvnItem::Number(1)]),
                ]),
            )
            .await;

            let failure_line = read_line(&mut server).await;

            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("abort-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;

            (failure_line, server)
        });

        let mut handler = Failer;
        let status = drive_editor_async(&mut conn, Some(&mut handler), false)
            .await
            .unwrap();
        assert!(matches!(
            status,
            EditorDriveStatus::Aborted(SvnError::Protocol(msg)) if msg == "boom"
        ));

        let (failure_line, mut server) = server_task.await.unwrap();
        assert_eq!(failure_line, encode_line(&expected_failure));

        let no_response =
            tokio::time::timeout(Duration::from_millis(50), read_line(&mut server)).await;
        assert!(no_response.is_err());
    });
}

#[test]
fn drive_editor_async_sends_failure_instead_of_success_on_close_edit_handler_error() {
    run_async(async {
        let (mut conn, mut server) = connected_conn().await;

        struct Failer;

        impl AsyncEditorEventHandler for Failer {
            fn on_event<'a>(
                &'a mut self,
                event: EditorEvent,
            ) -> Pin<Box<dyn Future<Output = Result<(), SvnError>> + Send + 'a>> {
                Box::pin(async move {
                    if matches!(event, EditorEvent::CloseEdit) {
                        return Err(SvnError::Protocol("boom".into()));
                    }
                    Ok(())
                })
            }
        }

        let expected_failure = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::String(b"protocol error: boom".to_vec()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);

        let server_task = tokio::spawn(async move {
            write_item_line(
                &mut server,
                &SvnItem::List(vec![
                    SvnItem::Word("close-edit".to_string()),
                    SvnItem::List(Vec::new()),
                ]),
            )
            .await;
            read_line(&mut server).await
        });

        let mut handler = Failer;
        let status = drive_editor_async(&mut conn, Some(&mut handler), false)
            .await
            .unwrap();
        assert!(matches!(
            status,
            EditorDriveStatus::Aborted(SvnError::Protocol(msg)) if msg == "boom"
        ));

        let response_line = server_task.await.unwrap();
        assert_eq!(response_line, encode_line(&expected_failure));
    });
}
