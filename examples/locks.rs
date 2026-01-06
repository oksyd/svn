//! Locks example (`get-lock(s)`, `lock`, `unlock`, and `*-many`).
//!
//! Write operations are disabled by default; set `SVN_WRITE=1` to opt in.

use std::time::Duration;

use svn::{
    Depth, LockManyOptions, LockOptions, LockTarget, RaSvnClient, SvnError, SvnUrl,
    UnlockManyOptions, UnlockOptions, UnlockTarget,
};

fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
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
            eprintln!("Read: SVN_LOCKS_DIR=trunk");
            eprintln!("Write: SVN_WRITE=1 SVN_LOCK_PATHS=trunk/file1.txt,trunk/file2.txt");
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

    let dir = std::env::var("SVN_LOCKS_DIR").unwrap_or_default();
    let locks = session.get_locks(&dir, Depth::Infinity).await?;
    println!("get-locks '{}' -> {} lock(s)", dir, locks.len());
    for lock in locks.iter().take(20) {
        println!("- {} owner={} token={}", lock.path, lock.owner, lock.token);
    }

    if std::env::var("SVN_WRITE").ok().as_deref() != Some("1") {
        return Ok(());
    }

    let paths = parse_csv_env("SVN_LOCK_PATHS")
        .ok_or_else(|| SvnError::Protocol("missing SVN_LOCK_PATHS".into()))?;
    let targets: Vec<LockTarget> = paths.iter().map(|p| LockTarget::new(p.clone())).collect();

    let results = session.lock_many(&LockManyOptions::new(), &targets).await?;
    let mut acquired = Vec::new();
    for (path, result) in paths.iter().zip(results.into_iter()) {
        match result {
            Ok(lock) => {
                println!("locked {} token={}", path, lock.token);
                acquired.push(lock);
            }
            Err(err) => eprintln!("lock failed for {path}: {err}"),
        }
    }

    let unlock_targets: Vec<UnlockTarget> = acquired
        .iter()
        .map(|l| UnlockTarget::new(l.path.clone()).with_token(l.token.clone()))
        .collect();
    let unlock_results = session
        .unlock_many(&UnlockManyOptions::new(), &unlock_targets)
        .await?;
    for (target, result) in unlock_targets.iter().zip(unlock_results.into_iter()) {
        match result {
            Ok(path) => println!("unlocked {path}"),
            Err(err) => eprintln!("unlock failed for {}: {err}", target.path),
        }
    }

    // Also demonstrate single-path APIs.
    if let Some(path) = paths.first() {
        let lock = session
            .lock(path, &LockOptions::new().with_comment("svn-rs lock"))
            .await?;
        session
            .unlock(path, &UnlockOptions::new().with_token(lock.token))
            .await?;
        println!("locked+unlocked {}", path);
    }

    Ok(())
}
