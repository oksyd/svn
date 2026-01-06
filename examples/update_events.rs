//! `update` editor drive example: record textdeltas and apply them.
//!
//! This uses an "empty" report (`start_empty = true`) and records in-memory
//! svndiff chunks via [`svn::TextDeltaRecorder`].

use std::time::Duration;

use svn::{
    Depth, RaSvnClient, Report, ReportCommand, SvnError, SvnUrl, TextDeltaRecorder, UpdateOptions,
    apply_textdelta_sync,
};

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
            eprintln!("Optional: SVN_TARGET=trunk SVN_REV=123 SVN_DEPTH=infinity");
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

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(60))
        .with_write_timeout(Duration::from_secs(60))
        .with_reconnect_retries(2);

    let mut session = client.open_session().await?;
    let head = session.get_latest_rev().await?;
    let rev = parse_u64_env("SVN_REV")?.or(Some(head));

    let mut options = UpdateOptions::new(target.clone(), depth);
    if let Some(rev) = rev {
        options = options.with_rev(rev);
    }

    let mut report = Report::new();
    report.push(ReportCommand::SetPath {
        path: String::new(),
        rev: 0,
        start_empty: true,
        lock_token: None,
        depth,
    });
    report.finish();

    let mut recorder = TextDeltaRecorder::new();
    session.update(&options, &report, &mut recorder).await?;

    let deltas = recorder.take_completed();
    println!("recorded {} textdelta stream(s)", deltas.len());

    let limit = parse_u64_env("SVN_MAX_FILES")?.unwrap_or(10) as usize;
    for delta in deltas.iter().take(limit) {
        let path = delta.path.as_deref().unwrap_or("<unknown>");
        let mut out = Vec::new();
        match apply_textdelta_sync(&[], delta.chunks.iter(), &mut out) {
            Ok(()) => println!("- {path}: {} bytes", out.len()),
            Err(err) => println!("- {path}: apply failed: {err}"),
        }
    }

    Ok(())
}
