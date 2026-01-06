//! Custom transport example using `open_session_with_stream`.
//!
//! Note: sessions created this way do not auto-reconnect, because the crate
//! cannot recreate your custom stream.

use std::time::Duration;

use svn::RaSvnClient;
use svn::SvnUrl;

fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

async fn run() -> svn::Result<()> {
    let url = match std::env::var("SVN_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("Set SVN_URL=svn://host/repo (optional SVN_USERNAME/SVN_PASSWORD).");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url.clone(), username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30));

    let addr = url.socket_addr();
    let stream = tokio::net::TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;

    let mut session = client.open_session_with_stream(stream).await?;
    let head = session.get_latest_rev().await?;
    println!("HEAD r{head}");

    let reconnect_err = session.reconnect().await.err();
    if let Some(err) = reconnect_err {
        println!("reconnect() is disabled for this session: {err}");
    }

    Ok(())
}
