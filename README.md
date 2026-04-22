<h1 align="center"><code>svn-rs</code></h1>

<p align="center">Async Rust SVN client for Subversion <code>svn://</code>, <code>svn+ssh://</code>, and <code>ra_svn</code> workflows.</p>

<div align="center">
  <a href="https://crates.io/crates/svn">
    <img src="https://img.shields.io/crates/v/svn.svg" alt="crates.io version">
  </a>
  <a href="https://docs.rs/svn">
    <img src="https://img.shields.io/docsrs/svn?logo=rust" alt="docs.rs docs">
  </a>
  <a href="https://github.com/lvillis/svn-rs/actions">
    <img src="https://github.com/lvillis/svn-rs/actions/workflows/ci.yaml/badge.svg" alt="CI status">
  </a>
  <a href="rust-toolchain.toml">
    <img src="https://img.shields.io/badge/MSRV-1.95.0-informational" alt="MSRV 1.95.0">
  </a>
</div>

`svn-rs` is an async Subversion client library for Rust. It talks to
`svnserve` over `svn://`, optionally tunnels `svn+ssh://`, and exposes
`ra_svn` read, report/editor, lock, and commit APIs. It is not a working copy
implementation.

## Highlights

- Async-first `RaSvnClient` and `RaSvnSession`
- Subversion `svn://` support, plus optional `svn+ssh://`
- `ra_svn` read operations, report/editor drives, locks, revision property
  updates, and low-level commit support
- Structured server errors with code, message, file, line, and command context
- Optional `serde`, `cyrus-sasl`, and `ssh` features

## Install

```toml
[dependencies]
svn = "0.1"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

Optional features:

```toml
svn = { version = "0.1", features = ["serde", "ssh", "cyrus-sasl"] }
```

## Example

```rust,no_run
use std::time::Duration;
use svn::{RaSvnClient, SvnUrl};

#[tokio::main]
async fn main() -> svn::Result<()> {
    let url = SvnUrl::parse("svn://example.com/repo")?;

    let client = RaSvnClient::new(url, None, None)
        .with_connect_timeout(Duration::from_secs(10))
        .with_read_timeout(Duration::from_secs(30))
        .with_write_timeout(Duration::from_secs(30));

    let mut session = client.open_session().await?;
    let head = session.get_latest_rev().await?;
    println!("HEAD = {head}");

    Ok(())
}
```

## Supported Operations

- Read: revisions, files, directories, logs, locations, mergeinfo, properties,
  file revs, locks
- Report/editor flows: `update`, `switch`, `status`, `diff`, `replay`,
  `replay-range`
- Write: revision property changes, lock/unlock, and commit editor commands

Not included:

- Working copy management
- Native TLS for `svn://`
- Full OpenSSH feature parity

## Authentication And Transport

Built-in `svn://` mechanisms:

- `ANONYMOUS`
- `PLAIN`
- `CRAM-MD5`

`cyrus-sasl` enables Cyrus SASL auth and negotiated SASL security layers.
`ssh` enables `svn+ssh://` by running `svnserve -t` over SSH.

## Examples

Examples live in [`examples/`](examples):

- `readonly`
- `get_file`
- `list`
- `export`
- `log_retry`
- `commit`
- `locks`
- `ssh`
- `sasl`

## Development

Run tests:

```bash
cargo test --all-features
```

Interop tests against a real `svnserve`:

```bash
SVN_INTEROP=1 cargo test --all-features --test interop_svnserve -- --nocapture
```

API docs: https://docs.rs/svn
