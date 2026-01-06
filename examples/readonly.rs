//! Read-only smoke example using a single `ra_svn` session.

use std::time::Duration;

use svn::{LogOptions, RaSvnClient, SvnUrl};

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
            eprintln!("Optional: SVN_TARGET=trunk, SVN_FILE=trunk/README.md");
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
    let head = session.get_latest_rev().await?;
    println!("HEAD r{head}");

    let listing = session.list_dir("", Some(head)).await?;
    println!("Root @r{}:", listing.rev);
    for entry in listing.entries.iter().take(20) {
        println!("{} {}", entry.kind, entry.name);
    }

    let target = match std::env::var("SVN_TARGET") {
        Ok(target) => target,
        Err(_) => "".to_string(),
    };
    let mut options = LogOptions::between(head, head.saturating_sub(5));
    options.target_paths = vec![target];
    options.changed_paths = false;

    let entries = session.log_with_options(&options).await?;
    for entry in entries {
        let author = entry.author.as_deref().unwrap_or("-");
        let message = entry.message.as_deref().unwrap_or("");
        println!(
            "r{} {} {}",
            entry.rev,
            author,
            message.lines().next().unwrap_or("")
        );
    }

    if let Ok(path) = std::env::var("SVN_FILE") {
        match session.get_file_bytes(&path, head, 1_048_576).await {
            Ok(bytes) => println!("Read {} bytes from {}", bytes.len(), path),
            Err(err) => eprintln!("get_file_bytes({path}) failed: {err}"),
        }
    }

    Ok(())
}
