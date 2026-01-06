//! Connection pooling and concurrency limiting with [`svn::SessionPool`].

use std::time::Duration;

use svn::{RaSvnClient, SessionPool, SessionPoolConfig, SessionPoolHealthCheck, SvnError, SvnUrl};

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

    let config = SessionPoolConfig::new(4)?
        .with_idle_timeout(Duration::from_secs(30))
        .with_health_check(SessionPoolHealthCheck::OnCheckoutIfIdleFor(
            Duration::from_secs(10),
        ))
        .with_prewarm_sessions(2);

    let pool = SessionPool::with_config(client, config)?;
    let created = pool.warm_up().await?;
    println!("prewarmed {created} session(s)");

    let mut handles = Vec::new();
    for worker in 0..8usize {
        let pool = pool.clone();
        let handle: tokio::task::JoinHandle<svn::Result<u64>> = tokio::spawn(async move {
            let mut session = pool.session().await?;
            let rev = session.get_latest_rev().await?;
            println!("worker {worker}: HEAD r{rev}");
            Ok(rev)
        });
        handles.push(handle);
    }

    for handle in handles {
        handle
            .await
            .map_err(|err| SvnError::Protocol(format!("worker task failed: {err}")))??;
    }

    Ok(())
}
