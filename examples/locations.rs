//! History/location helpers: `get-locations`, `get-location-segments`, and `get-deleted-rev`.

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
            eprintln!("Required: SVN_PATH=trunk/path");
            eprintln!("Optional: SVN_PEG_REV=HEAD, SVN_START_REV=1, SVN_END_REV=HEAD");
            return Ok(());
        }
    };
    let path = match std::env::var("SVN_PATH") {
        Ok(path) => path,
        Err(_) => {
            eprintln!("Set SVN_PATH=trunk/path");
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

    let peg_rev = parse_u64_env("SVN_PEG_REV")?.unwrap_or(head);
    let start_rev = parse_u64_env("SVN_START_REV")?.unwrap_or(1);
    let end_rev = parse_u64_env("SVN_END_REV")?.unwrap_or(head);

    let location_revs = [peg_rev, start_rev, end_rev];
    let locations = session
        .get_locations(&path, peg_rev, &location_revs)
        .await?;
    println!("get-locations for '{path}' @peg r{peg_rev}:");
    for loc in locations {
        println!("- r{} -> {}", loc.rev, loc.path);
    }

    println!();
    let segments = session
        .get_location_segments(&path, peg_rev, Some(start_rev), Some(end_rev))
        .await?;
    println!("get-location-segments:");
    for seg in segments {
        println!(
            "- r{}..=r{} -> {}",
            seg.range_start,
            seg.range_end,
            seg.path.as_deref().unwrap_or("(gap)")
        );
    }

    println!();
    let deleted = session.get_deleted_rev(&path, peg_rev, end_rev).await?;
    match deleted {
        Some(r) => println!("get-deleted-rev: deleted at r{r}"),
        None => println!("get-deleted-rev: not deleted (or missing revision)"),
    }

    Ok(())
}
