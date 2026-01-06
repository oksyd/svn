//! `get-file` streaming example.
//!
//! Required:
//! - `SVN_URL=svn://host/repo`
//! - `SVN_FILE=trunk/path/to/file`
//!
//! Optional:
//! - `SVN_USERNAME` / `SVN_PASSWORD`
//! - `SVN_REV=123` (defaults to HEAD)
//! - `SVN_DEST=/path/to/output` (write contents to a file; otherwise discard)
//! - `SVN_MAX_BYTES=1048576`
//! - `SVN_PROPS=1` (request file props)
//! - `SVN_IPROPS=1` (request inherited props)

use std::time::Duration;

use svn::{GetFileOptions, RaSvnClient, SvnError, SvnUrl};
use tokio::io::AsyncWriteExt;

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

fn env_is_1(name: &str) -> bool {
    std::env::var(name).ok().as_deref() == Some("1")
}

fn normalize_rel_path(input: String) -> String {
    input.trim_start_matches('/').to_string()
}

fn print_props(label: &str, props: &svn::PropertyList) {
    if props.is_empty() {
        println!("{label}: <none>");
        return;
    }
    println!("{label}:");
    for (name, value) in props {
        let preview = match std::str::from_utf8(value) {
            Ok(s) => {
                let mut out = String::new();
                for ch in s.chars().take(120) {
                    out.push(ch);
                }
                out.trim_end_matches(['\r', '\n']).to_string()
            }
            Err(_) => format!("<{} bytes>", value.len()),
        };
        println!("- {name} = {preview}");
    }
}

async fn run() -> svn::Result<()> {
    let url = match std::env::var("SVN_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("Set SVN_URL=svn://host/repo (optional SVN_USERNAME/SVN_PASSWORD).");
            eprintln!("Then set SVN_FILE=trunk/path/to/file (optional SVN_REV, SVN_DEST).");
            return Ok(());
        }
    };
    let file = match std::env::var("SVN_FILE") {
        Ok(path) => normalize_rel_path(path),
        Err(_) => {
            eprintln!("Set SVN_FILE=trunk/path/to/file");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let want_props = env_is_1("SVN_PROPS");
    let want_iprops = env_is_1("SVN_IPROPS");
    let max_bytes = parse_u64_env("SVN_MAX_BYTES")?.unwrap_or(1_048_576);

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(60))
        .with_write_timeout(Duration::from_secs(60))
        .with_reconnect_retries(2);

    let mut session = client.open_session().await?;
    let rev = match parse_u64_env("SVN_REV")? {
        Some(rev) => rev,
        None => session.get_latest_rev().await?,
    };

    let mut options = GetFileOptions::new(rev, max_bytes);
    if want_props {
        options = options.with_props();
    }
    if want_iprops {
        options = options.with_iprops();
    }

    let dest = std::env::var("SVN_DEST").ok();
    let result = if let Some(dest) = dest.as_deref() {
        let mut out = tokio::fs::File::create(dest).await?;
        let result = session
            .get_file_with_options(&file, &options, &mut out)
            .await?;
        out.flush().await?;
        out.sync_all().await?;
        println!("wrote {} bytes to {}", result.bytes_written, dest);
        result
    } else {
        let mut sink = tokio::io::sink();
        let result = session
            .get_file_with_options(&file, &options, &mut sink)
            .await?;
        println!("read {} bytes from {}", result.bytes_written, file);
        result
    };

    println!("served revision: r{}", result.rev);
    if let Some(checksum) = result.checksum.as_deref() {
        println!("checksum: {checksum}");
    }
    if want_props {
        print_props("props", &result.props);
    }
    if want_iprops {
        if result.inherited_props.is_empty() {
            println!("inherited props: <none>");
        } else {
            for entry in result.inherited_props {
                println!("inherited props for {}", entry.path);
                print_props("  props", &entry.props);
            }
        }
    }

    Ok(())
}
