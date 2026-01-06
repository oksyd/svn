//! Session reparenting (`reparent`) to switch the session's repository root path.

use std::time::Duration;

use svn::{RaSvnClient, SvnUrl};

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
            eprintln!("Set SVN_URL=svn://host/repo/path (optional SVN_USERNAME/SVN_PASSWORD).");
            eprintln!("Required: SVN_REPARENT_URL=svn://same-host/other-path");
            return Ok(());
        }
    };
    let new_url = match std::env::var("SVN_REPARENT_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("Set SVN_REPARENT_URL=svn://same-host/other-path");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30))
        .with_reconnect_retries(2);

    let mut session = client.open_session().await?;
    println!("before: {}", session.client().base_url().url);

    let new_url = SvnUrl::parse(&new_url)?;
    session.reparent(new_url).await?;
    println!("after:  {}", session.client().base_url().url);

    let head = session.get_latest_rev().await?;
    println!("HEAD r{head}");
    Ok(())
}
