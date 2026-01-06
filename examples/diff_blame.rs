//! High-level unified diff and blame helpers.

use std::time::Duration;

use svn::{RaSvnClient, SvnError, SvnUrl};

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
            eprintln!("Then set SVN_FILE=trunk/path/to/file.txt");
            return Ok(());
        }
    };
    let path = match std::env::var("SVN_FILE") {
        Ok(path) => path,
        Err(_) => {
            eprintln!("Set SVN_FILE=trunk/path/to/file.txt");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let max_bytes = parse_u64_env("SVN_MAX_BYTES")?.unwrap_or(1_048_576);

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30))
        .with_reconnect_retries(2);

    let mut session = client.open_session().await?;
    let head = session.get_latest_rev().await?;
    if head < 1 {
        eprintln!("repository is at r{head}; need at least r1 for diff/blame");
        return Ok(());
    }

    let old_rev = head.saturating_sub(1);
    let diff = session
        .diff_file_unified(&path, old_rev, head, max_bytes)
        .await?;
    println!("{diff}");

    let start_rev = head.saturating_sub(20);
    let blame = session
        .blame_file(&path, Some(start_rev), Some(head), false, max_bytes)
        .await?;
    for line in blame.iter().take(30) {
        let author = line.author.as_deref().unwrap_or("-");
        let content = line.line.trim_end_matches(['\r', '\n']);
        println!("r{:>6} {:<12} {content}", line.rev, author);
    }

    Ok(())
}
