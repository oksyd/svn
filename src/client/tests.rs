#![allow(clippy::unwrap_used)]

use super::commit_ops::encode_proplist;
use super::*;
use crate::rasvn::conn::RaSvnConnectionConfig;
use crate::svndiff::{SvndiffVersion, encode_fulltext_with_options};
use crate::test_support::{
    encode_line, read_until_newline as read_line, run_async, write_item_line,
};
use std::time::Duration;

mod commit;
mod data;
mod history;
mod list;
mod locks;
mod meta;
mod report;
mod transport;

async fn connected_session() -> (RaSvnSession, tokio::net::TcpStream) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let accept_task = tokio::spawn(async move { listener.accept().await });
    let client_stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let (server, _) = accept_task.await.unwrap().unwrap();

    let (read, write) = client_stream.into_split();
    let mut client = RaSvnClient::new(SvnUrl::parse("svn://example.com/repo").unwrap(), None, None);
    client.read_timeout = Duration::from_secs(1);
    client.write_timeout = Duration::from_secs(1);
    let conn = RaSvnConnection::new(
        Box::new(read),
        Box::new(write),
        RaSvnConnectionConfig {
            username: None,
            password: None,
            #[cfg(feature = "cyrus-sasl")]
            host: client.base_url.host.clone(),
            #[cfg(feature = "cyrus-sasl")]
            local_addrport: None,
            #[cfg(feature = "cyrus-sasl")]
            remote_addrport: None,
            url: client.base_url.url.clone(),
            is_tunneled: false,
            ra_client: client.ra_client.clone(),
            read_timeout: client.read_timeout,
            write_timeout: client.write_timeout,
        },
    );

    (
        RaSvnSession {
            client,
            conn: Some(conn),
            server_info: None,
            allow_reconnect: true,
        },
        server,
    )
}

fn auth_request(realm: &str) -> SvnItem {
    SvnItem::List(vec![
        SvnItem::Word("success".to_string()),
        SvnItem::List(vec![
            SvnItem::List(Vec::new()),
            SvnItem::String(realm.as_bytes().to_vec()),
        ]),
    ])
}

async fn handshake_no_auth(server: &mut tokio::net::TcpStream) {
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
    write_item_line(server, &greeting).await;

    let _client_greeting = read_line(server).await;

    write_item_line(server, &auth_request("realm")).await;

    let repos_info = SvnItem::List(vec![
        SvnItem::Word("success".to_string()),
        SvnItem::List(vec![
            SvnItem::String(b"uuid".to_vec()),
            SvnItem::String(b"svn://example.com/repo".to_vec()),
            SvnItem::List(Vec::new()),
        ]),
    ]);
    write_item_line(server, &repos_info).await;
}
