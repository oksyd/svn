//! Cyrus SASL authentication example (requires `--features cyrus-sasl`).
//!
//! Required:
//! - `SVN_URL=svn://host/repo`
//!
//! Optional:
//! - `SVN_USERNAME` / `SVN_PASSWORD`
//!
//! Notes:
//! - The `cyrus-sasl` feature uses a system-provided `libsasl2` at runtime.
//! - Whether SASL is used depends on what the server offers during auth.

#[cfg(not(feature = "cyrus-sasl"))]
fn main() {
    eprintln!("This example requires `--features cyrus-sasl`.");
    eprintln!("Example: cargo run --example sasl --features cyrus-sasl");
}

#[cfg(feature = "cyrus-sasl")]
fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

#[cfg(feature = "cyrus-sasl")]
async fn run() -> svn::Result<()> {
    use std::time::Duration;

    use svn::{RaSvnClient, SvnUrl};

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
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30))
        .with_reconnect_retries(2);

    let head = client.get_latest_rev().await?;
    println!("HEAD r{head}");
    Ok(())
}
