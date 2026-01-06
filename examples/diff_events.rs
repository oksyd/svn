//! `diff` editor drive example: count editor events.
//!
//! This uses an "empty" report (`start_empty = true`). Real diff behavior
//! against a working copy requires a detailed report; this example is mainly
//! for protocol interop and event handling.
//!
//! Required:
//! - `SVN_URL=svn://host/repo`
//! - `SVN_VERSUS_URL=svn://host/repo/branches/branch1`
//!
//! Optional:
//! - `SVN_USERNAME` / `SVN_PASSWORD`
//! - `SVN_TARGET=trunk` (defaults to repository root)
//! - `SVN_REV=123` (defaults to server default, usually HEAD)
//! - `SVN_DEPTH=infinity` (empty|files|immediates|infinity)
//! - `SVN_TEXT_DELTAS=0` (set to `0` to disable requesting text deltas)
//! - `SVN_IGNORE_ANCESTRY=1`

use std::time::Duration;

use svn::{
    Depth, DiffOptions, EditorEvent, EditorEventHandler, RaSvnClient, Report, ReportCommand,
    SvnError, SvnUrl,
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

fn env_is_1(name: &str) -> bool {
    std::env::var(name).ok().as_deref() == Some("1")
}

fn env_bool(name: &str, default: bool) -> Result<bool, SvnError> {
    let Ok(raw) = std::env::var(name) else {
        return Ok(default);
    };
    let v = raw.trim().to_ascii_lowercase();
    let parsed = match v.as_str() {
        "" => default,
        "1" | "true" | "yes" | "y" => true,
        "0" | "false" | "no" | "n" => false,
        _ => {
            return Err(SvnError::Protocol(format!(
                "invalid {name} '{raw}' (expected 1/0/true/false)"
            )));
        }
    };
    Ok(parsed)
}

fn normalize_rel_dir(input: String) -> String {
    input.trim_matches('/').to_string()
}

#[derive(Default)]
struct Counter {
    events: u64,
    dirs: u64,
    files: u64,
    deletes: u64,
    textdelta_chunks: u64,
    close_edit: u64,
    abort_edit: u64,
    sample_paths: Vec<String>,
}

impl Counter {
    fn record_path(&mut self, path: String) {
        const MAX: usize = 20;
        if self.sample_paths.len() < MAX {
            self.sample_paths.push(path);
        }
    }
}

impl EditorEventHandler for Counter {
    fn on_event(&mut self, event: EditorEvent) -> svn::Result<()> {
        self.events += 1;
        match event {
            EditorEvent::AddDir { path, .. } | EditorEvent::OpenDir { path, .. } => {
                self.dirs += 1;
                self.record_path(path);
            }
            EditorEvent::AddFile { path, .. } | EditorEvent::OpenFile { path, .. } => {
                self.files += 1;
                self.record_path(path);
            }
            EditorEvent::DeleteEntry { path, .. } => {
                self.deletes += 1;
                self.record_path(path);
            }
            EditorEvent::TextDeltaChunk { .. } => {
                self.textdelta_chunks += 1;
            }
            EditorEvent::CloseEdit => {
                self.close_edit += 1;
            }
            EditorEvent::AbortEdit => {
                self.abort_edit += 1;
            }
            EditorEvent::AbsentDir { path, .. } | EditorEvent::AbsentFile { path, .. } => {
                self.record_path(path);
            }
            _ => {}
        }
        Ok(())
    }
}

async fn run() -> svn::Result<()> {
    let url = match std::env::var("SVN_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("Set SVN_URL=svn://host/repo (optional SVN_USERNAME/SVN_PASSWORD).");
            eprintln!("Then set SVN_VERSUS_URL=svn://host/repo/branches/branch1");
            return Ok(());
        }
    };
    let versus_url = match std::env::var("SVN_VERSUS_URL") {
        Ok(url) => url,
        Err(_) => {
            eprintln!("Set SVN_VERSUS_URL=svn://host/repo/branches/branch1");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let target = normalize_rel_dir(std::env::var("SVN_TARGET").unwrap_or_default());
    let depth = match std::env::var("SVN_DEPTH") {
        Ok(value) => parse_depth(&value).ok_or_else(|| {
            SvnError::Protocol(format!(
                "invalid SVN_DEPTH '{value}' (expected empty|files|immediates|infinity)"
            ))
        })?,
        Err(_) => Depth::Infinity,
    };
    let rev = parse_u64_env("SVN_REV")?;
    let text_deltas = env_bool("SVN_TEXT_DELTAS", true)?;

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(60))
        .with_write_timeout(Duration::from_secs(60))
        .with_reconnect_retries(2);

    let mut session = client.open_session().await?;

    let options = if let Some(rev) = rev {
        DiffOptions::new(target, versus_url, depth)
            .with_rev(rev)
            .with_text_deltas(text_deltas)
    } else {
        DiffOptions::new(target, versus_url, depth).with_text_deltas(text_deltas)
    };
    let options = if env_is_1("SVN_IGNORE_ANCESTRY") {
        options.ignore_ancestry()
    } else {
        options
    };

    let mut report = Report::new();
    report.push(ReportCommand::SetPath {
        path: String::new(),
        rev: 0,
        start_empty: true,
        lock_token: None,
        depth,
    });
    report.finish();

    let mut counter = Counter::default();
    session.diff(&options, &report, &mut counter).await?;

    println!(
        "events={}, dirs={}, files={}, deletes={}, textdelta_chunks={}, close_edit={}, abort_edit={}",
        counter.events,
        counter.dirs,
        counter.files,
        counter.deletes,
        counter.textdelta_chunks,
        counter.close_edit,
        counter.abort_edit
    );
    for path in counter.sample_paths {
        println!("path: {path}");
    }
    Ok(())
}
