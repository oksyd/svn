#![allow(clippy::unwrap_used)]

use super::*;
use crate::test_support::{read_until_newline, run_async, write_item_line};
use proptest::prelude::*;
#[cfg(feature = "cyrus-sasl")]
use std::sync::{Arc, Mutex};

async fn connected_conn_inner(
    username: Option<String>,
    password: Option<String>,
    is_tunneled: bool,
) -> (RaSvnConnection, tokio::net::TcpStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept_task = tokio::spawn(async move { listener.accept().await });
    let client = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (server, _) = accept_task.await.unwrap().unwrap();

    let (read, write) = client.into_split();
    let conn = RaSvnConnection::new(
        Box::new(read),
        Box::new(write),
        RaSvnConnectionConfig {
            username,
            password,
            #[cfg(feature = "cyrus-sasl")]
            host: "example.com".to_string(),
            #[cfg(feature = "cyrus-sasl")]
            local_addrport: None,
            #[cfg(feature = "cyrus-sasl")]
            remote_addrport: None,
            is_tunneled,
            url: if is_tunneled {
                "svn+ssh://example.com:22/repo".to_string()
            } else {
                "svn://example.com:3690/repo".to_string()
            },
            ra_client: "test-ra_svn".to_string(),
            read_timeout: Duration::from_secs(1),
            write_timeout: Duration::from_secs(1),
        },
    );

    (conn, server)
}

async fn connected_conn(
    username: Option<String>,
    password: Option<String>,
) -> (RaSvnConnection, tokio::net::TcpStream) {
    connected_conn_inner(username, password, false).await
}

async fn connected_conn_tunneled(
    username: Option<String>,
    password: Option<String>,
) -> (RaSvnConnection, tokio::net::TcpStream) {
    connected_conn_inner(username, password, true).await
}

fn arb_word() -> impl Strategy<Value = String> {
    "[A-Za-z_][A-Za-z0-9_\\-]{0,31}"
        .prop_filter("avoid bool words", |w| w != "true" && w != "false")
}

fn arb_item() -> impl Strategy<Value = SvnItem> {
    let leaf = prop_oneof![
        arb_word().prop_map(SvnItem::Word),
        any::<u64>().prop_map(SvnItem::Number),
        any::<bool>().prop_map(SvnItem::Bool),
        prop::collection::vec(any::<u8>(), 0..64).prop_map(SvnItem::String),
    ];
    leaf.prop_recursive(6, 256, 12, |inner| {
        prop::collection::vec(inner, 0..16).prop_map(SvnItem::List)
    })
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 64,
        .. ProptestConfig::default()
    })]

    #[test]
    fn encode_then_read_roundtrips(item in arb_item()) {
        run_async(async {
            let (mut conn, mut server) = connected_conn(None, None).await;

            let mut encoded = Vec::new();
            encode_item(&item, &mut encoded);
            encoded.push(b'\n');

            server.write_all(&encoded).await.unwrap();
            server.flush().await.unwrap();

            let parsed = conn.read_item().await.unwrap();
            assert_eq!(parsed, item);
        });
    }
}

#[cfg(feature = "cyrus-sasl")]
fn invert_bytes(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().map(|b| b ^ 0xFF).collect()
}

#[cfg(feature = "cyrus-sasl")]
fn chunk_sizes(total: usize, max: usize) -> Vec<usize> {
    if total == 0 {
        return Vec::new();
    }
    if max == 0 {
        return vec![total];
    }
    let mut out = Vec::new();
    let mut remaining = total;
    while remaining > 0 {
        let take = remaining.min(max);
        out.push(take);
        remaining -= take;
    }
    out
}

#[cfg(feature = "cyrus-sasl")]
struct DummySecurityLayer {
    max: u32,
    encode_calls: Arc<Mutex<Vec<usize>>>,
    decode_calls: Arc<Mutex<Vec<usize>>>,
}

#[cfg(feature = "cyrus-sasl")]
impl SaslSecurityLayer for DummySecurityLayer {
    fn max_outbuf(&self) -> u32 {
        self.max
    }

    fn encode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError> {
        self.encode_calls.lock().unwrap().push(input.len());
        Ok(invert_bytes(input))
    }

    fn decode(&mut self, input: &[u8]) -> Result<Vec<u8>, SvnError> {
        self.decode_calls.lock().unwrap().push(input.len());
        Ok(invert_bytes(input))
    }
}

#[test]
fn select_mech_prefers_plain_over_anonymous() {
    run_async(async {
        let (conn, _server) =
            connected_conn(Some("alice".to_string()), Some("secret".to_string())).await;
        let mechs = vec!["ANONYMOUS".to_string(), "PLAIN".to_string()];
        let (mech, token) = conn.select_mech(&mechs).unwrap();
        assert_eq!(mech, "PLAIN");
        let mut expected = Vec::new();
        expected.push(0);
        expected.extend_from_slice(b"alice");
        expected.push(0);
        expected.extend_from_slice(b"secret");
        assert_eq!(token.unwrap(), expected);
    });
}

#[test]
fn select_mech_uses_cram_md5_when_plain_missing() {
    run_async(async {
        let (conn, _server) =
            connected_conn(Some("alice".to_string()), Some("secret".to_string())).await;
        let mechs = vec!["CRAM-MD5".to_string(), "ANONYMOUS".to_string()];
        let (mech, token) = conn.select_mech(&mechs).unwrap();
        assert_eq!(mech, "CRAM-MD5");
        assert!(token.is_none());
    });
}

#[test]
fn select_mech_falls_back_to_anonymous_without_creds() {
    run_async(async {
        let (conn, _server) = connected_conn(None, None).await;
        let mechs = vec!["ANONYMOUS".to_string()];
        let (mech, token) = conn.select_mech(&mechs).unwrap();
        assert_eq!(mech, "ANONYMOUS");
        assert_eq!(token.unwrap(), Vec::<u8>::new());
    });
}

#[test]
fn select_mech_prefers_external_when_tunneled() {
    run_async(async {
        let (conn, _server) = connected_conn_tunneled(None, None).await;
        let mechs = vec!["ANONYMOUS".to_string(), "EXTERNAL".to_string()];
        let (mech, token) = conn.select_mech(&mechs).unwrap();
        assert_eq!(mech, "EXTERNAL");
        assert_eq!(token.unwrap(), Vec::<u8>::new());
    });
}

#[test]
fn select_mech_reports_unavailable_when_no_supported_mechs() {
    run_async(async {
        let (conn, _server) = connected_conn(None, None).await;
        let mechs = vec!["PLAIN".to_string(), "CRAM-MD5".to_string()];
        let err = conn.select_mech(&mechs).unwrap_err();
        assert!(matches!(err, SvnError::AuthUnavailable));
    });
}

#[test]
fn auth_request_retries_with_next_mechanism_on_failure() {
    run_async(async {
        let (mut conn, mut server) =
            connected_conn(Some("alice".to_string()), Some("secret".to_string())).await;

        let server_task = tokio::spawn(async move {
            let auth_request = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(vec![
                        SvnItem::Word("CRAM-MD5".to_string()),
                        SvnItem::Word("PLAIN".to_string()),
                    ]),
                    SvnItem::String(b"realm".to_vec()),
                ]),
            ]);
            write_item_line(&mut server, &auth_request).await;

            let first_response = read_until_newline(&mut server).await;
            let expected_first = SvnItem::List(vec![
                SvnItem::Word("CRAM-MD5".to_string()),
                SvnItem::List(Vec::new()),
            ]);
            let mut expected_first_bytes = Vec::new();
            encode_item(&expected_first, &mut expected_first_bytes);
            expected_first_bytes.push(b'\n');
            assert_eq!(first_response, expected_first_bytes);

            let failure = SvnItem::List(vec![
                SvnItem::Word("failure".to_string()),
                SvnItem::List(vec![SvnItem::String(b"bad".to_vec())]),
            ]);
            write_item_line(&mut server, &failure).await;

            let second_response = read_until_newline(&mut server).await;
            let mut token = Vec::new();
            token.push(0);
            token.extend_from_slice(b"alice");
            token.push(0);
            token.extend_from_slice(b"secret");
            let expected_second = SvnItem::List(vec![
                SvnItem::Word("PLAIN".to_string()),
                SvnItem::List(vec![SvnItem::String(token)]),
            ]);
            let mut expected_second_bytes = Vec::new();
            encode_item(&expected_second, &mut expected_second_bytes);
            expected_second_bytes.push(b'\n');
            assert_eq!(second_response, expected_second_bytes);

            write_item_line(
                &mut server,
                &SvnItem::List(vec![SvnItem::Word("success".to_string())]),
            )
            .await;
        });

        conn.handle_auth_request().await.unwrap();
        server_task.await.unwrap();
    });
}

#[test]
fn auth_step_reply_cram_md5_matches_known_vector() {
    run_async(async {
        let (conn, _server) =
            connected_conn(Some("alice".to_string()), Some("key".to_string())).await;
        let reply = conn
            .auth_step_reply(
                "CRAM-MD5",
                b"The quick brown fox jumps over the lazy dog".to_vec(),
            )
            .unwrap();
        let reply = String::from_utf8(reply).unwrap();
        assert_eq!(reply, "alice 80070713463e7749b90c2dc24911e275");
    });
}

#[test]
fn auth_step_reply_cram_md5_requires_creds() {
    run_async(async {
        let (conn, _server) = connected_conn(None, Some("key".to_string())).await;
        let err = conn
            .auth_step_reply("CRAM-MD5", b"challenge".to_vec())
            .unwrap_err();
        assert!(matches!(err, SvnError::AuthFailed(_)));

        let (conn, _server) = connected_conn(Some("alice".to_string()), None).await;
        let err = conn
            .auth_step_reply("CRAM-MD5", b"challenge".to_vec())
            .unwrap_err();
        assert!(matches!(err, SvnError::AuthFailed(_)));
    });
}

#[test]
fn read_item_roundtrips_encoded_values() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;
        let item = SvnItem::List(vec![
            SvnItem::Word("word".to_string()),
            SvnItem::Number(22),
            SvnItem::Bool(true),
            SvnItem::String(b"bytes".to_vec()),
            SvnItem::List(vec![SvnItem::Word("nested".to_string())]),
        ]);
        write_item_line(&mut server, &item).await;
        let parsed = conn.read_item().await.unwrap();
        assert_eq!(parsed, item);
    });
}

#[test]
fn read_item_rejects_invalid_word_tokens() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;
        server.write_all(b"wo(rd ").await.unwrap();
        server.flush().await.unwrap();
        let err = conn.read_item().await.unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
    });
}

#[test]
fn read_item_rejects_number_overflow() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;
        server.write_all(b"18446744073709551616 \n").await.unwrap();
        server.flush().await.unwrap();
        let err = conn.read_item().await.unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
    });
}

#[test]
fn read_item_requires_whitespace_after_strings() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;
        server.write_all(b"4:testX \n").await.unwrap();
        server.flush().await.unwrap();
        let err = conn.read_item().await.unwrap_err();
        assert!(matches!(err, SvnError::Protocol(msg) if msg == "expected whitespace"));
    });
}

#[test]
fn handshake_writes_expected_client_greeting() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;

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

            let client_greeting = read_until_newline(&mut server).await;
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
            let mut expected_bytes = Vec::new();
            encode_item(&expected, &mut expected_bytes);
            expected_bytes.push(b'\n');
            assert_eq!(client_greeting, expected_bytes);

            let auth_request = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(Vec::new()),
                    SvnItem::String(b"realm".to_vec()),
                ]),
            ]);
            write_item_line(&mut server, &auth_request).await;

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

        let info = conn.handshake().await.unwrap();
        assert!(conn.server_has_cap("edit-pipeline"));
        assert!(conn.server_has_cap("svndiff1"));
        assert_eq!(info.repository.uuid, "uuid");
        assert_eq!(info.repository.root_url, "svn://example.com/repo");
        assert!(
            info.repository
                .capabilities
                .iter()
                .any(|c| c == "mergeinfo")
        );
        server_task.await.unwrap();
    });
}

#[test]
fn handshake_skips_leading_garbage_for_tunneled_connections() {
    run_async(async {
        let (mut conn, mut server) = connected_conn_tunneled(None, None).await;

        let server_task = tokio::spawn(async move {
            server
                .write_all(b"Last login: Thu Jan 01 00:00:00 1970\\n")
                .await
                .unwrap();
            server.flush().await.unwrap();

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

            let client_greeting = read_until_newline(&mut server).await;
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
                SvnItem::String(b"svn+ssh://example.com:22/repo".to_vec()),
                SvnItem::String(b"test-ra_svn".to_vec()),
                SvnItem::List(Vec::new()),
            ]);
            let mut expected_bytes = Vec::new();
            encode_item(&expected, &mut expected_bytes);
            expected_bytes.push(b'\n');
            assert_eq!(client_greeting, expected_bytes);

            let auth_request = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(vec![
                    SvnItem::List(Vec::new()),
                    SvnItem::String(b"realm".to_vec()),
                ]),
            ]);
            write_item_line(&mut server, &auth_request).await;

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

        let info = conn.handshake().await.unwrap();
        assert_eq!(info.repository.uuid, "uuid");
        server_task.await.unwrap();
    });
}

#[test]
fn handshake_rejects_servers_without_v2_support() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;
        let server_task = tokio::spawn(async move {
            let greeting = SvnItem::List(vec![
                SvnItem::Word("success".to_string()),
                SvnItem::List(vec![
                    SvnItem::Number(3),
                    SvnItem::Number(4),
                    SvnItem::List(Vec::new()),
                    SvnItem::List(Vec::new()),
                ]),
            ]);
            write_item_line(&mut server, &greeting).await;
        });

        let err = conn.handshake().await.unwrap_err();
        assert!(matches!(err, SvnError::Protocol(_)));
        server_task.await.unwrap();
    });
}

#[cfg(feature = "cyrus-sasl")]
#[test]
fn write_item_applies_security_layer_and_chunks() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;

        let encode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
        let decode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
        conn.sasl = Some(Box::new(DummySecurityLayer {
            max: 8,
            encode_calls: encode_calls.clone(),
            decode_calls,
        }));

        let item = SvnItem::List(vec![
            SvnItem::Word("test".to_string()),
            SvnItem::String(vec![b'x'; 25]),
        ]);
        let mut plain = Vec::new();
        encode_item(&item, &mut plain);
        plain.push(b'\n');

        conn.write_item(&item).await.unwrap();

        let mut got = vec![0u8; plain.len()];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(got, invert_bytes(&plain));
        assert_eq!(*encode_calls.lock().unwrap(), chunk_sizes(plain.len(), 8));
    });
}

#[cfg(feature = "cyrus-sasl")]
#[test]
fn write_item_security_layer_skips_chunking_when_max_outbuf_zero() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;

        let encode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
        let decode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
        conn.sasl = Some(Box::new(DummySecurityLayer {
            max: 0,
            encode_calls: encode_calls.clone(),
            decode_calls,
        }));

        let item = SvnItem::String(vec![b'a'; 10]);
        let mut plain = Vec::new();
        encode_item(&item, &mut plain);
        plain.push(b'\n');

        conn.write_item(&item).await.unwrap();

        let mut got = vec![0u8; plain.len()];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(got, invert_bytes(&plain));
        assert_eq!(*encode_calls.lock().unwrap(), vec![plain.len()]);
    });
}

#[cfg(feature = "cyrus-sasl")]
#[test]
fn read_item_decodes_with_security_layer() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;

        let encode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
        let decode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
        conn.sasl = Some(Box::new(DummySecurityLayer {
            max: 0,
            encode_calls,
            decode_calls: decode_calls.clone(),
        }));

        let item = SvnItem::List(vec![
            SvnItem::Word("hello".to_string()),
            SvnItem::Number(1),
            SvnItem::String(b"world".to_vec()),
        ]);
        let mut plain = Vec::new();
        encode_item(&item, &mut plain);
        plain.push(b'\n');
        let wire = invert_bytes(&plain);
        server.write_all(&wire).await.unwrap();
        server.flush().await.unwrap();

        let parsed = conn.read_item().await.unwrap();
        assert_eq!(parsed, item);

        let decoded_total: usize = decode_calls.lock().unwrap().iter().sum();
        assert_eq!(decoded_total, wire.len());
    });
}

#[cfg(feature = "cyrus-sasl")]
#[test]
fn write_cmd_failure_early_applies_security_layer_and_chunks() {
    run_async(async {
        let (mut conn, mut server) = connected_conn(None, None).await;

        let encode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
        let decode_calls = Arc::new(Mutex::new(Vec::<usize>::new()));
        conn.sasl = Some(Box::new(DummySecurityLayer {
            max: 7,
            encode_calls: encode_calls.clone(),
            decode_calls,
        }));

        let err = SvnError::Protocol("boom".into());
        let done = conn.write_cmd_failure_early(&err).await.unwrap();
        assert!(!done);

        let item = SvnItem::List(vec![
            SvnItem::Word("failure".to_string()),
            SvnItem::List(vec![SvnItem::List(vec![
                SvnItem::Number(1),
                SvnItem::String(err.to_string().into_bytes()),
                SvnItem::String(Vec::new()),
                SvnItem::Number(0),
            ])]),
        ]);

        let mut plain = Vec::new();
        encode_item(&item, &mut plain);
        plain.push(b'\n');

        let mut got = vec![0u8; plain.len()];
        server.read_exact(&mut got).await.unwrap();
        assert_eq!(got, invert_bytes(&plain));
        assert_eq!(*encode_calls.lock().unwrap(), chunk_sizes(plain.len(), 7));
    });
}
