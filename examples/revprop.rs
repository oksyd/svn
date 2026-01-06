//! Revision property read/write (`rev-prop`, `rev-proplist`, `change-rev-prop*`).
//!
//! Writing revision properties usually requires the server to allow it (e.g. a
//! `pre-revprop-change` hook). This example is read-only by default; set
//! `SVN_WRITE=1` to opt in.

use std::time::Duration;

use svn::{Capability, RaSvnClient, SvnError, SvnUrl};

fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

fn parse_u64_env(name: &str) -> Result<Option<u64>, SvnError> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let value = raw
        .parse::<u64>()
        .map_err(|_| SvnError::Protocol(format!("invalid {name} '{raw}'")))?;
    Ok(Some(value))
}

async fn run() -> svn::Result<()> {
    let url = match std::env::var("SVN_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("Set SVN_URL=svn://host/repo (optional SVN_USERNAME/SVN_PASSWORD).");
            eprintln!("Optional: SVN_REV=123");
            eprintln!("Write: SVN_WRITE=1 SVN_REVPROP_NAME=name SVN_REVPROP_VALUE=value");
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
    let rev = parse_u64_env("SVN_REV")?.unwrap_or(head);

    let author = session.rev_prop(rev, "svn:author").await?;
    let author = author.map(|v| String::from_utf8_lossy(&v).into_owned());
    println!(
        "r{rev} svn:author = {}",
        author.unwrap_or_else(|| "-".to_string())
    );

    let revprops = session.rev_proplist(rev).await?;
    println!("r{rev} has {} revprop(s)", revprops.len());

    if std::env::var("SVN_WRITE").ok().as_deref() != Some("1") {
        return Ok(());
    }

    let name = std::env::var("SVN_REVPROP_NAME")
        .map_err(|_| SvnError::Protocol("missing SVN_REVPROP_NAME".into()))?;
    let value = std::env::var("SVN_REVPROP_VALUE")
        .unwrap_or_default()
        .into_bytes();

    if session.has_capability(Capability::AtomicRevProps) {
        session
            .change_rev_prop2(rev, &name, Some(value), true, None)
            .await?;
    } else {
        session.change_rev_prop(rev, &name, Some(value)).await?;
    }

    println!("updated r{rev} {name}");
    Ok(())
}
