//! Mergeinfo example (`get-mergeinfo`).

use std::time::Duration;

use svn::{Capability, MergeInfoInheritance, RaSvnClient, SvnError, SvnUrl};

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

fn parse_csv_env(name: &str) -> Option<Vec<String>> {
    let raw = std::env::var(name).ok()?;
    let parts: Vec<String> = raw
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    (!parts.is_empty()).then_some(parts)
}

async fn run() -> svn::Result<()> {
    let url = match std::env::var("SVN_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("Set SVN_URL=svn://host/repo (optional SVN_USERNAME/SVN_PASSWORD).");
            eprintln!("Optional: SVN_MERGEINFO_PATHS=trunk,branches/feature-x SVN_REV=123");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let paths = parse_csv_env("SVN_MERGEINFO_PATHS").unwrap_or_else(|| vec!["".to_string()]);

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30))
        .with_reconnect_retries(2);

    let mut session = client.open_session().await?;
    if !session.has_capability(Capability::MergeInfo) {
        println!("server does not support get-mergeinfo");
        return Ok(());
    }

    let head = session.get_latest_rev().await?;
    let rev = parse_u64_env("SVN_REV")?.or(Some(head));
    let catalog = session
        .get_mergeinfo(&paths, rev, MergeInfoInheritance::NearestAncestor, true)
        .await?;

    println!("get-mergeinfo returned {} entr(ies):", catalog.len());
    for (path, mergeinfo) in catalog.iter().take(50) {
        println!("- {path}: {mergeinfo}");
    }
    Ok(())
}
