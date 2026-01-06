//! Resolve a date string to a revision (`get-dated-rev`).

use std::time::Duration;

use svn::{RaSvnClient, SvnError, SvnUrl};

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
            eprintln!("Required: SVN_DATE=2025-01-01T00:00:00.000000Z");
            return Ok(());
        }
    };
    let date = match std::env::var("SVN_DATE") {
        Ok(date) => date,
        Err(_) => return Err(SvnError::Protocol("missing SVN_DATE".into())),
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
    let rev = session.get_dated_rev(&date).await?;
    println!("{date} -> r{rev}");
    Ok(())
}
