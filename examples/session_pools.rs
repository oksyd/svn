//! Multi-repository pooling with [`svn::SessionPools`].
//!
//! This is useful when you talk to multiple `svn://` servers and want:
//! - bounded concurrency,
//! - connection reuse,
//! - and automatic `reparent` for different URL paths on the same host.
//!
//! Required:
//! - One of:
//!   - `SVN_URLS=svn://host/repo1,svn://host/repo2` (comma-separated), or
//!   - `SVN_URL=svn://host/repo` (and optional `SVN_URL2=...`)
//!
//! Optional:
//! - `SVN_USERNAME` / `SVN_PASSWORD`
//! - `SVN_POOL_MAX=8` (max concurrent sessions per pool)
//! - `SVN_POOL_KEY=tenant-a` (partition pools by a custom key)

use std::time::Duration;

use svn::{RaSvnClient, SessionPoolConfig, SessionPoolHealthCheck, SessionPools, SvnError, SvnUrl};

fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

fn parse_usize_env(name: &str) -> Result<Option<usize>, SvnError> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(None);
    };
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let value = raw
        .parse::<usize>()
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
    let mut urls = if let Some(urls) = parse_csv_env("SVN_URLS") {
        urls
    } else if let Ok(url) = std::env::var("SVN_URL") {
        let mut urls = vec![url];
        if let Ok(url2) = std::env::var("SVN_URL2")
            && !url2.trim().is_empty()
        {
            urls.push(url2);
        }
        urls
    } else {
        eprintln!("Set SVN_URLS=svn://host/repo1,svn://host/repo2");
        eprintln!("Or set SVN_URL=svn://host/repo (optional SVN_URL2=...).");
        return Ok(());
    };

    urls.sort();
    urls.dedup();

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();
    let pool_key = std::env::var("SVN_POOL_KEY")
        .ok()
        .filter(|s| !s.trim().is_empty());
    let max_sessions = parse_usize_env("SVN_POOL_MAX")?.unwrap_or(8);

    let config = SessionPoolConfig::new(max_sessions)?
        .with_idle_timeout(Duration::from_secs(30))
        .with_health_check(SessionPoolHealthCheck::OnCheckoutIfIdleFor(
            Duration::from_secs(10),
        ));
    let pools = SessionPools::new(config);

    let mut handles = Vec::new();
    for (idx, url) in urls.into_iter().enumerate() {
        let pools = pools.clone();
        let username = username.clone();
        let password = password.clone();
        let pool_key = pool_key.clone();
        let handle: tokio::task::JoinHandle<svn::Result<(usize, String, u64)>> =
            tokio::spawn(async move {
                let url_parsed = SvnUrl::parse(&url)?;
                let client = RaSvnClient::new(url_parsed, username, password)
                    .with_connect_timeout(Duration::from_secs(10))
                    .with_read_timeout(Duration::from_secs(60))
                    .with_write_timeout(Duration::from_secs(60))
                    .with_reconnect_retries(2);

                let mut session = if let Some(key) = pool_key {
                    pools.session_with_key(client, key).await?
                } else {
                    pools.session(client).await?
                };

                let head = session.get_latest_rev().await?;
                Ok((idx, url, head))
            });
        handles.push(handle);
    }

    for handle in handles {
        let (idx, url, head) = handle
            .await
            .map_err(|err| SvnError::Protocol(format!("task join failed: {err}")))??;
        println!("[{idx}] {url}: HEAD r{head}");
    }

    Ok(())
}
