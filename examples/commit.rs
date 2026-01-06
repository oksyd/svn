//! Write example using [`svn::CommitBuilder`].
//!
//! This example performs a real commit. It is disabled by default; set
//! `SVN_WRITE=1` to opt in.

use std::time::Duration;

use svn::{CommitBuilder, CommitOptions, CommitStreamBuilder, RaSvnClient, SvnError, SvnUrl};

fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

async fn run() -> svn::Result<()> {
    if std::env::var("SVN_WRITE").ok().as_deref() != Some("1") {
        eprintln!("This example is disabled by default.");
        eprintln!("Set SVN_WRITE=1 to allow committing.");
        eprintln!("Required: SVN_URL, SVN_COMMIT_PATH, SVN_COMMIT_MESSAGE.");
        eprintln!("Use either: SVN_COMMIT_CONTENT or SVN_LOCAL_FILE.");
        return Ok(());
    }

    let url = match std::env::var("SVN_URL") {
        Ok(url) => url,
        Err(_) => return Err(SvnError::Protocol("missing SVN_URL".into())),
    };
    let repo_path = match std::env::var("SVN_COMMIT_PATH") {
        Ok(path) => path,
        Err(_) => return Err(SvnError::Protocol("missing SVN_COMMIT_PATH".into())),
    };
    let message = match std::env::var("SVN_COMMIT_MESSAGE") {
        Ok(message) => message,
        Err(_) => return Err(SvnError::Protocol("missing SVN_COMMIT_MESSAGE".into())),
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(60))
        .with_write_timeout(Duration::from_secs(60))
        .with_reconnect_retries(2);

    let mut session = client.open_session().await?;
    let options = CommitOptions::new(message);

    let info = if let Ok(local_file) = std::env::var("SVN_LOCAL_FILE") {
        let file = tokio::fs::File::open(local_file).await?;
        let reader = tokio::io::BufReader::new(file);
        let builder = CommitStreamBuilder::new().put_file_reader(repo_path, reader);
        session
            .commit_with_stream_builder(&options, builder)
            .await?
    } else {
        let content = std::env::var("SVN_COMMIT_CONTENT")
            .unwrap_or_else(|_| "hello from svn-rs\n".to_string())
            .into_bytes();
        let head = session.get_latest_rev().await?;
        let builder = CommitBuilder::new()
            .with_base_rev(head)
            .put_file(repo_path, content);
        session.commit_with_builder(&options, &builder).await?
    };

    println!("committed r{}", info.new_rev);
    if let Some(err) = info.post_commit_err.as_deref() {
        eprintln!("post-commit error: {err}");
    }
    Ok(())
}
