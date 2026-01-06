//! `svn+ssh://` transport example (requires `--features ssh`).

#[cfg(not(feature = "ssh"))]
fn main() {
    eprintln!("This example requires `--features ssh`.");
    eprintln!("Example: cargo run --example ssh --features ssh");
}

#[cfg(feature = "ssh")]
fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

#[cfg(feature = "ssh")]
async fn run() -> svn::Result<()> {
    use std::time::Duration;

    use svn::{RaSvnClient, SshConfig, SvnUrl};

    let url = match std::env::var("SVN_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("Set SVN_URL=svn+ssh://host/repo (optional SVN_USERNAME/SVN_PASSWORD).");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_ssh_config(SshConfig::default())
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30))
        .with_reconnect_retries(2);

    let head = client.get_latest_rev().await?;
    println!("HEAD r{head}");
    Ok(())
}
