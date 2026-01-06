//! Properties example: `proplist`, `propget`, `get-iprops`, and `rev-prop(s)`.

use std::time::Duration;

use svn::{Capability, RaSvnClient, SvnError, SvnUrl};

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
            eprintln!("Optional: SVN_PATH=trunk, SVN_REV=123, SVN_PROP=svn:eol-style");
            return Ok(());
        }
    };

    let username = std::env::var("SVN_USERNAME").ok();
    let password = std::env::var("SVN_PASSWORD").ok();

    let path = std::env::var("SVN_PATH").unwrap_or_default();
    let prop_name = std::env::var("SVN_PROP").unwrap_or_else(|_| "svn:eol-style".to_string());

    let url = SvnUrl::parse(&url)?;
    let client = RaSvnClient::new(url, username, password)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30))
        .with_reconnect_retries(2);

    let mut session = client.open_session().await?;
    let head = session.get_latest_rev().await?;
    let rev = parse_u64_env("SVN_REV")?.unwrap_or(head);

    println!("proplist: '{}' @r{rev}", path);
    match session.proplist(&path, Some(rev)).await? {
        Some(props) => {
            for (name, value) in props {
                let text = String::from_utf8_lossy(&value);
                println!("- {name} = {text:?} ({} bytes)", value.len());
            }
        }
        None => println!("(node does not exist)"),
    }

    println!();
    println!("propget: '{prop_name}'");
    let value = session.propget(&path, Some(rev), &prop_name).await?;
    match value {
        Some(bytes) => println!("{}", String::from_utf8_lossy(&bytes)),
        None => println!("(not set)"),
    }

    println!();
    if session.has_capability(Capability::InheritedProps) {
        let iprops = session.inherited_props(&path, Some(rev)).await?;
        println!("get-iprops: {} entr(ies)", iprops.len());
        for entry in iprops.iter().take(10) {
            println!("- {} ({} prop(s))", entry.path, entry.props.len());
        }
    } else {
        println!("get-iprops: not supported by server");
    }

    println!();
    let author = session.rev_prop(rev, "svn:author").await?;
    let author = author.map(|v| String::from_utf8_lossy(&v).into_owned());
    println!(
        "rev-prop svn:author @r{rev}: {}",
        author.unwrap_or_else(|| "-".to_string())
    );

    let revprops = session.rev_proplist(rev).await?;
    println!("rev-proplist @r{rev}: {} prop(s)", revprops.len());

    Ok(())
}
