# svn

[![Crates.io version](https://img.shields.io/crates/v/svn.svg)](https://crates.io/crates/svn)
[![API documentation](https://img.shields.io/docsrs/svn?logo=rust)](https://docs.rs/svn)
[![CI status](https://github.com/oksyd/svn/actions/workflows/ci.yaml/badge.svg)](https://github.com/oksyd/svn/actions/workflows/ci.yaml)
[![MSRV 1.100.0](https://img.shields.io/badge/MSRV-1.100.0-informational)](Cargo.toml)

An async Subversion client library for Rust, built on Tokio. Read repository
contents and history, export directories, and commit changes over `svn://` or
`svn+ssh://` (with the `ssh` feature).

[Crates.io](https://crates.io/crates/svn) · [API documentation](https://docs.rs/svn) ·
[Examples](examples/README.md) · [CI](https://github.com/oksyd/svn/actions/workflows/ci.yaml)

Requires Rust 1.100.0 or newer. Supports remote repository access; working copy
management and `http://`, `https://`, and `file://` transports are outside its scope.

## Usage

```toml
[dependencies]
svn = "0.1"
tokio = { version = "1", features = ["rt"] }
```

Reuse a session for multiple operations on the same connection:

```rust,no_run
use svn::{RaSvnClient, SvnUrl};

fn main() -> svn::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async {
        let client = RaSvnClient::new(SvnUrl::parse("svn://example.com/repo")?, None, None);
        let mut session = client.open_session().await?;
        let revision = session.get_latest_rev().await?;

        // An empty path selects the session URL; pin the revision for this read.
        let listing = session.list_dir("", Some(revision)).await?;
        for entry in listing.entries {
            println!("{} {}", entry.kind, entry.path);
        }
        Ok(())
    })
}
```

See the [crate guide](https://docs.rs/svn/latest/svn/) for API selection, path and
revision conventions, authentication, and error handling.

## Optional features

No optional features are enabled by default.

| Feature | Purpose |
| --- | --- |
| `ssh` | `svn+ssh://` through an SSH connection running `svnserve -t` |
| `serde` | Serialization and deserialization for supported public data types |
| `cyrus-sasl` | Additional authentication mechanisms and negotiated security layers; requires `libsasl2` and mechanism plugins at runtime |

For example: `svn = { version = "0.1", features = ["ssh"] }`.

## License

[Apache-2.0](LICENSE)
