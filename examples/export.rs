//! Export a repository subtree to a local directory.

use std::path::PathBuf;
use std::time::Duration;

use svn::{Depth, RaSvnClient, SvnError, SvnUrl, UpdateOptions};

fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
}

fn parse_depth(input: &str) -> Option<Depth> {
    match input.trim().to_ascii_lowercase().as_str() {
        "empty" => Some(Depth::Empty),
        "files" => Some(Depth::Files),
        "immediates" => Some(Depth::Immediates),
        "infinity" | "infinite" => Some(Depth::Infinity),
        _ => None,
    }
}

async fn run() -> svn::Result<()> {
    let url = match std::env::var("SVN_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("Set SVN_URL=svn://host/repo (optional SVN_USERNAME/SVN_PASSWORD).");
            eprintln!("Then set SVN_DEST=/path/to/output-dir (and optional SVN_TARGET, SVN_REV).");
            return Ok(());
        }
    };
    let dest = match std::env::var("SVN_DEST") {
        Ok(dest) => PathBuf::from(dest),
        Err(_) => {
            eprintln!("Set SVN_DEST=/path/to/output-dir");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let target = std::env::var("SVN_TARGET").unwrap_or_default();
    let depth = match std::env::var("SVN_DEPTH") {
        Ok(value) => parse_depth(&value).ok_or_else(|| {
            SvnError::Protocol(format!(
                "invalid SVN_DEPTH '{value}' (expected empty|files|immediates|infinity)"
            ))
        })?,
        Err(_) => Depth::Infinity,
    };
    let mut options = UpdateOptions::new(target, depth);
    if let Ok(rev) = std::env::var("SVN_REV") {
        let rev: u64 = rev
            .parse()
            .map_err(|_| SvnError::Protocol(format!("invalid SVN_REV '{rev}'")))?;
        options = options.with_rev(rev);
    }

    tokio::fs::create_dir_all(&dest).await?;

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(60))
        .with_write_timeout(Duration::from_secs(60))
        .with_reconnect_retries(2);

    client.export_to_dir(&options, &dest).await?;
    println!("exported to {}", dest.display());
    Ok(())
}
