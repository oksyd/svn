//! `replay-range` example: stream revision properties and editor events.

use std::time::Duration;

use svn::{EditorEvent, EditorEventHandler, RaSvnClient, ReplayRangeOptions, SvnError, SvnUrl};

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
    events: usize,
    revprops: usize,
    textdelta_chunks: usize,
}

impl EditorEventHandler for Counter {
    fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
        self.events += 1;
        match event {
            EditorEvent::RevProps { .. } => self.revprops += 1,
            EditorEvent::TextDeltaChunk { .. } => self.textdelta_chunks += 1,
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
            eprintln!("Optional: SVN_START_REV=1 SVN_END_REV=HEAD SVN_LOW_WATER=0");
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
    let head = session.get_latest_rev().await?;

    let start_rev = parse_u64_env("SVN_START_REV")?.unwrap_or(head.saturating_sub(3));
    let end_rev = parse_u64_env("SVN_END_REV")?.unwrap_or(head);
    let low_water = parse_u64_env("SVN_LOW_WATER")?.unwrap_or(0);

    let options = ReplayRangeOptions::new(start_rev, end_rev).with_low_water_mark(low_water);
    let mut counter = Counter::default();
    session.replay_range(&options, &mut counter).await?;

    println!(
        "replay-range r{start_rev}..=r{end_rev}: events={} revprops={} textdelta_chunks={}",
        counter.events, counter.revprops, counter.textdelta_chunks
    );
    Ok(())
}
