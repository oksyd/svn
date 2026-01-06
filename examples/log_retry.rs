//! Resumable `log` streaming with automatic reconnect and deduplication.

use std::time::Duration;

use svn::{LogOptions, LogRevProps, RaSvnClient, SvnUrl};

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
            eprintln!("Optional: SVN_TARGET=trunk");
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
        .with_reconnect_retries(3);

    let head = client.get_latest_rev().await?;
    let target = match std::env::var("SVN_TARGET") {
        Ok(target) => target,
        Err(_) => "".to_string(),
    };

    let mut options = LogOptions::between(head, head.saturating_sub(50));
    options.target_paths = vec![target];
    options.changed_paths = false;
    options.revprops = LogRevProps::Custom(vec![
        "svn:author".to_string(),
        "svn:date".to_string(),
        "svn:log".to_string(),
    ]);

    client
        .log_each_retrying(&options, |entry| {
            let author = entry.author.as_deref().unwrap_or("-");
            let message = entry.message.as_deref().unwrap_or("");
            println!(
                "r{} {} {}",
                entry.rev,
                author,
                message.lines().next().unwrap_or("")
            );
            Ok(())
        })
        .await?;

    Ok(())
}
