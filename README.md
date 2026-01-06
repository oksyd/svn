<div align="right">

<span style="color:#999;">🇺🇸 English</span> ·
<a href="README.zh-CN.md">🇨🇳 中文</a> &nbsp;&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;|&nbsp;&nbsp;&nbsp;&nbsp;&nbsp;
<a href="#table-of-contents">Table of Contents</a>

</div>

<h1 align="center"><code>svn-rs</code></h1>

<p align="center">A Rust async client for Subversion <code>svn://</code> (<code>ra_svn</code>) — a modern alternative to <code>libsvn_ra_svn</code>.</p>

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
    <img src="https://img.shields.io/badge/MSRV-1.92.0-informational" alt="MSRV 1.92.0">
  </a>
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-MIT-blue.svg" alt="license">
  </a>
  <a href="https://crates.io/crates/svn">
    <img src="https://img.shields.io/crates/dr/svn?color=ba86eb" alt="downloads">
  </a>
</div>

---

<a id="table-of-contents"></a>

## Features

- Async-first `svn://` (`ra_svn`) client (no working copy).
- Optional `svn+ssh://` via SSH tunnel (`ssh` feature; runs `svnserve -t` over SSH).
- High-level API: `RaSvnClient` / `RaSvnSession`.
- Structured server errors (`code/message/file/line`) with command context.
- `serde` feature for public data types.
- Optional `cyrus-sasl` feature for Cyrus SASL auth + negotiated SASL security
  layer (requires a system-provided `libsasl2` at runtime).

## Installation

Add the crate:

```bash
cargo add svn
```

Or in `Cargo.toml`:

```toml
[dependencies]
svn = "0.1"
```

You also need an async runtime. The examples below use `tokio`:

```toml
[dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

## Quick start

The `RaSvnClient` type is a reusable configuration; use it to create a connected
`RaSvnSession` (which owns a single TCP connection).

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
    let latest = session.get_latest_rev().await?;
    println!("HEAD = {latest}");

    Ok(())
}
```

## Examples

All examples are in `examples/` and are intended to be copy-pasteable.

- Read-only smoke: `SVN_URL=... cargo run --example readonly`
- Resumable log stream: `SVN_URL=... cargo run --example log_retry`
- Fetch a file (+ props/iprops): `SVN_URL=... SVN_FILE=... cargo run --example get_file`
- Directory listing: `SVN_URL=... cargo run --example list`
- Export a subtree to disk: `SVN_URL=... SVN_DEST=... cargo run --example export`
- Pooling: `SVN_URL=... cargo run --example pool` / `SVN_URLS=... cargo run --example session_pools`
- Editor drives: `update_events`, `status_events`, `diff_events`, `switch_events`, `replay_events`, `replay_range_events`
- Write operations (opt-in): `SVN_WRITE=1 cargo run --example commit` / `SVN_WRITE=1 cargo run --example locks`
- Optional features: `cargo run --example ssh --features ssh`, `cargo run --example sasl --features cyrus-sasl`

## Configuration

`RaSvnClient` is cheap to clone and provides builder-style methods:

- `with_connect_timeout(Duration)`
- `with_read_timeout(Duration)`
- `with_write_timeout(Duration)`
- `with_ra_client(String)` (sent during handshake)

In general, prefer reusing a single `RaSvnSession` for multiple operations to
avoid repeated reconnect + handshake.

## Authentication

This crate supports `svn://` authentication mechanisms commonly offered by
`svnserve`:

- `ANONYMOUS`
- `PLAIN` (username + password)
- `CRAM-MD5` (username + password)

Pass `username`/`password` when creating `RaSvnClient`. If the server requires an
unsupported mechanism, operations return `SvnError::AuthUnavailable`.

To enable Cyrus SASL (and the optional SASL security layer), enable the
`cyrus-sasl` feature (requires `libsasl2` installed on the system at runtime):

```toml
svn = { version = "0.1", features = ["cyrus-sasl"] }
```

Notes:

- With `cyrus-sasl`, this crate dynamically loads the system Cyrus SASL library
  (`libsasl2`) at runtime. If it is not available, SASL authentication is
  unavailable and requests may fail with `SvnError::AuthUnavailable`.
- The SASL security layer is not TLS (no certificates); it is an optional
  integrity/encryption layer negotiated as part of SASL, depending on the
  mechanism and server configuration.
- When `cyrus-sasl` is disabled (the default), the crate stays `unsafe`-free.

## `svn+ssh://` (SSH tunnel)

Enable the `ssh` feature to connect using `svn+ssh://` URLs (runs `svnserve -t`
over SSH using `russh`):

```toml
svn = { version = "0.1", features = ["ssh"] }
```

```rust,no_run
use std::path::PathBuf;
use svn::{RaSvnClient, SshAuth, SshConfig, SvnUrl};

# #[tokio::main] async fn main() -> svn::Result<()> {
let url = SvnUrl::parse("svn+ssh://example.com/repo")?;
let ssh = SshConfig::new(SshAuth::KeyFile {
    path: PathBuf::from("~/.ssh/id_ed25519"),
    passphrase: None,
});

let client = RaSvnClient::new(url, None, None).with_ssh_config(ssh);
let mut session = client.open_session().await?;
let head = session.get_latest_rev().await?;
println!("{head}");
# Ok(()) }
```

## Supported operations

This crate focuses on `ra_svn` protocol v2 and currently supports:

- Read: `get-latest-rev`, `get-dated-rev`, `get-file`, `get-dir`, `log`, `list`,
  `check-path`, `stat`, `get-mergeinfo`, `get-deleted-rev`, `get-locations`,
  `get-location-segments`, `get-file-revs`, `rev-prop`, `rev-proplist`,
  `proplist`, `propget`, `get-iprops`, locks listing (`get-lock`, `get-locks`).
- Report/editor flows: `update`, `switch`, `status`, `diff`, `replay`,
  `replay-range`.
- Write: `change-rev-prop`, `change-rev-prop2`, `lock`/`unlock` (including
  `*-many`), and a low-level `commit` API driven by `EditorCommand`.

For full API docs and examples, see https://docs.rs/svn.

## Errors

All APIs return `svn::Result<T>` (an alias for `Result<T, SvnError>`). Server-side
failures are returned as `SvnError::Server(ServerError)` and include a structured
error chain and command context.

```rust,no_run
use svn::{RaSvnClient, SvnError, SvnUrl};

#[tokio::main]
async fn main() {
    let url = SvnUrl::parse("svn://example.com/repo").unwrap();
    let client = RaSvnClient::new(url, None, None);

    let err = client.get_latest_rev().await.unwrap_err();
    match err {
        SvnError::Server(server) => {
            eprintln!("server: {server}");
            for item in &server.chain {
                eprintln!("  code={} msg={:?} file={:?} line={:?}", item.code, item.message, item.file, item.line);
            }
        }
        other => eprintln!("error: {other}"),
    }
}
```

## Logging

This crate uses `tracing` for debug logging. Enable logs in your application
(for example with `tracing-subscriber`) and set an appropriate filter:

```text
RUST_LOG=svn=debug
```

## Compatibility

- Protocol: `ra_svn` v2 (`svn://`, and `svn+ssh://` with the `ssh` feature).
- IPv6: supported via bracketed URLs (for example `svn://[::1]/repo`).
- MSRV: Rust `1.92.0` (see `Cargo.toml`).
- Optional `serde` support via the `serde` feature.
- Optional Cyrus SASL support via `cyrus-sasl` (runtime `libsasl2`).
- Optional SSH tunnel support via `ssh` (`russh`).

## Security

- `svn://` is plain TCP (no native TLS).
- `svn+ssh://` uses SSH for transport encryption and authentication.
- `PLAIN` sends credentials without encryption unless you use a secure tunnel
  (VPN / SSH port forwarding / stunnel) or negotiate a SASL security layer.
- Even with `CRAM-MD5`, repository traffic is still unencrypted unless a tunnel
  or SASL security layer is used.

## Testing

Unit tests and property tests:

```bash
cargo test --all-features
```

Interop tests against a real `svnserve` (requires `svn`, `svnadmin`, `svnserve`):

```bash
SVN_INTEROP=1 cargo test --all-features --test interop_svnserve -- --nocapture
```

## Limitations

- Not a working copy client (no checkout / update of a local working copy).
- No native TLS (see [Security](#security)).
- The `ssh` feature supports a subset of `~/.ssh/config` and ssh-agent (no ProxyJump/ProxyCommand).

## License

Licensed under the MIT license. See `LICENSE`.
