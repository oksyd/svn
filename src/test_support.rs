#![allow(clippy::unwrap_used)]

use std::future::Future;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::rasvn::encode_item;
use crate::raw::SvnItem;

pub(crate) fn run_async<T>(f: impl Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(f)
}

pub(crate) fn encode_line(item: &SvnItem) -> Vec<u8> {
    let mut buf = Vec::new();
    encode_item(item, &mut buf);
    buf.push(b'\n');
    buf
}

pub(crate) async fn write_item_line(stream: &mut tokio::net::TcpStream, item: &SvnItem) {
    let buf = encode_line(item);
    stream.write_all(&buf).await.unwrap();
    stream.flush().await.unwrap();
}

pub(crate) async fn read_until_newline(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await.unwrap();
        if n == 0 {
            break;
        }
        buf.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    buf
}
