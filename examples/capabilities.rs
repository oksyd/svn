//! Prints repository metadata and negotiated server capabilities.

use std::time::Duration;

use svn::{Capability, RaSvnClient, SvnUrl};

fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(run())
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
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30))
        .with_reconnect_retries(2);

    let session = client.open_session().await?;
    let Some(info) = session.server_info() else {
        return Ok(());
    };

    println!("repository uuid: {}", info.repository.uuid);
    println!("repository root: {}", info.repository.root_url);
    println!("client base_url:  {}", session.client().base_url().url);

    println!();
    println!("capabilities:");
    for cap in [
        Capability::EditPipeline,
        Capability::Svndiff1,
        Capability::AcceptsSvndiff2,
        Capability::AbsentEntries,
        Capability::CommitRevProps,
        Capability::MergeInfo,
        Capability::Depth,
        Capability::AtomicRevProps,
        Capability::InheritedProps,
        Capability::LogRevProps,
        Capability::PartialReplay,
        Capability::EphemeralTxnProps,
        Capability::GetFileRevsReverse,
        Capability::List,
    ] {
        if session.has_capability(cap) {
            println!("- {}", cap.as_wire_word());
        }
    }

    Ok(())
}
