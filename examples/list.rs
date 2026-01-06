//! Directory listing examples (`get-dir`, `list`, and `list_recursive`).

use std::time::Duration;

use svn::{Capability, Depth, DirentField, RaSvnClient, SvnError, SvnUrl};

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
            eprintln!("Optional: SVN_PATH=trunk, SVN_REV=123, SVN_PATTERNS=*.rs,*.md");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let path = std::env::var("SVN_PATH").unwrap_or_default();
    let rev = parse_u64_env("SVN_REV")?;
    let patterns = parse_csv_env("SVN_PATTERNS");

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30))
        .with_reconnect_retries(2);

    let mut session = client.open_session().await?;
    let listing = session.list_dir(&path, rev).await?;
    println!("get-dir: '{}' @r{}", path, listing.rev);
    for entry in listing.entries.iter().take(20) {
        println!("{} {}", entry.kind, entry.path);
    }

    if session.has_capability(Capability::List) {
        println!();
        println!("list capability is available");
        let fields = [
            DirentField::Kind,
            DirentField::Size,
            DirentField::HasProps,
            DirentField::CreatedRev,
            DirentField::Time,
            DirentField::LastAuthor,
        ];
        let entries = session
            .list(&path, rev, Depth::Immediates, &fields, patterns.as_deref())
            .await?;
        println!("list: {} entr(ies)", entries.len());
        for entry in entries.iter().take(20) {
            println!("{} {}", entry.kind, entry.path);
        }
    } else {
        println!();
        println!("list capability is not available");
    }

    println!();
    let recursive = session.list_recursive(&path, rev).await?;
    println!("list_recursive: {} entr(ies)", recursive.len());
    Ok(())
}
