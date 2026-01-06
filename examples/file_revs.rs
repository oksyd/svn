//! `get-file-revs` + applying raw svndiff chunks.

use std::time::Duration;

use svn::{RaSvnClient, SvnError, SvnUrl, apply_textdelta_sync};

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
            eprintln!("Required: SVN_FILE=trunk/path/to/file.txt");
            eprintln!("Optional: SVN_START_REV=1, SVN_END_REV=HEAD, SVN_MAX_BYTES=1048576");
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

    let start_rev = parse_u64_env("SVN_START_REV")?.unwrap_or(head.saturating_sub(10));
    let end_rev = parse_u64_env("SVN_END_REV")?.unwrap_or(head);

    let file_revs = session
        .get_file_revs(&path, Some(start_rev), Some(end_rev), false)
        .await?;
    println!("get-file-revs returned {} entr(ies)", file_revs.len());

    let mut current: Vec<u8> = Vec::new();
    for rev in &file_revs {
        let next = if rev.delta_chunks.is_empty() {
            current.clone()
        } else {
            let mut out = Vec::new();
            apply_textdelta_sync(&current, rev.delta_chunks.iter(), &mut out)?;
            out
        };
        println!(
            "r{} chunks={} bytes={}",
            rev.rev,
            rev.delta_chunks.len(),
            next.len()
        );
        current = next;
    }

    println!();
    let with_contents = session
        .get_file_revs_with_contents(&path, Some(start_rev), Some(end_rev), false, max_bytes)
        .await?;
    let last = with_contents
        .last()
        .ok_or_else(|| SvnError::Protocol("empty get_file_revs_with_contents result".into()))?;
    println!(
        "get_file_revs_with_contents last: r{} bytes={}",
        last.file_rev.rev,
        last.contents.len()
    );

    Ok(())
}
