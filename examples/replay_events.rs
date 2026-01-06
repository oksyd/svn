//! `replay` editor drive example: count events for a single revision.
//!
//! Required:
//! - `SVN_URL=svn://host/repo`
//!
//! Optional:
//! - `SVN_USERNAME` / `SVN_PASSWORD`
//! - `SVN_REV=123` (defaults to HEAD)

use std::time::Duration;

use svn::{EditorEvent, EditorEventHandler, RaSvnClient, ReplayOptions, SvnError, SvnUrl};

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

#[derive(Default)]
struct Counter {
    events: u64,
    dirs: u64,
    files: u64,
    deltas: u64,
    revprops: u64,
    finish_replay: u64,
}

impl EditorEventHandler for Counter {
    fn on_event(&mut self, event: EditorEvent) -> svn::Result<()> {
        self.events += 1;
        match event {
            EditorEvent::AddDir { .. }
            | EditorEvent::OpenDir { .. }
            | EditorEvent::OpenRoot { .. } => {
                self.dirs += 1;
            }
            EditorEvent::AddFile { .. } | EditorEvent::OpenFile { .. } => {
                self.files += 1;
            }
            EditorEvent::TextDeltaChunk { .. } => {
                self.deltas += 1;
            }
            EditorEvent::RevProps { .. } => {
                self.revprops += 1;
            }
            EditorEvent::FinishReplay => {
                self.finish_replay += 1;
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
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

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

    let options = ReplayOptions::new(rev);
    let mut counter = Counter::default();
    session.replay(&options, &mut counter).await?;

    println!("replayed r{rev}");
    println!(
        "events={}, dirs={}, files={}, textdelta_chunks={}, revprops={}, finish_replay={}",
        counter.events,
        counter.dirs,
        counter.files,
        counter.deltas,
        counter.revprops,
        counter.finish_replay
    );
    Ok(())
}
