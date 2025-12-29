//! Async client for Subversion's `svn://` (`ra_svn`) protocol.
//!
//! This crate implements a subset of Subversion's remote access protocol used by
//! `svnserve` (the `svn://` scheme). It is a network client and does **not**
//! implement a working copy.
//!
//! Most users should start with [`RaSvnClient`] to create a connected
//! [`RaSvnSession`].
//!
//! ## Getting started
//!
//! ```rust,no_run
//! use std::time::Duration;
//! use svn::{RaSvnClient, SvnUrl};
//!
//! fn main() -> svn::Result<()> {
//!     let rt = tokio::runtime::Builder::new_current_thread()
//!         .enable_all()
//!         .build()?;
//!
//!     rt.block_on(async {
//!         let url = SvnUrl::parse("svn://example.com/repo")?;
//!         let client = RaSvnClient::new(url, None, None)
//!             .with_read_timeout(Duration::from_secs(30));
//!
//!         // A session reuses one connection and caches server info.
//!         let mut session = client.open_session().await?;
//!         let latest = session.get_latest_rev().await?;
//!         println!("{latest}");
//!         Ok(())
//!     })
//! }
//! ```
//!
//! ## Features
//!
//! - `serde`: enables `Serialize`/`Deserialize` for public data types.
//! - `cyrus-sasl`: enables Cyrus SASL authentication and (when negotiated)
//!   the SASL security layer (requires a system-provided `libsasl2` at runtime).
//!
//! ## Protocol notes
//!
//! - Only `svn://` is supported (no `svn+ssh://`).
//! - IPv6 URLs must use brackets (for example `svn://[::1]/repo`).
//! - Built-in authentication mechanisms: `ANONYMOUS`, `PLAIN`, and `CRAM-MD5`.
//!   With `cyrus-sasl`, the client can also use Cyrus SASL (including the
//!   optional SASL security layer when negotiated).
//!
//! ## Custom transports
//!
//! If you need to bring your own transport (for example a custom proxy/tunnel),
//! you can connect the stream yourself and then call
//! [`RaSvnClient::open_session_with_stream`].
//!
//! ## Low-level access
//!
//! For raw wire protocol items, see [`raw::SvnItem`].

#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]
#![deny(unsafe_code)]

mod client;
mod commit;
mod editor;
mod error;
mod export;
mod highlevel;
mod options;
mod path;
mod pool;
mod rasvn;
mod svndiff;
mod textdelta;
mod types;
mod url;

pub use client::{RaSvnClient, RaSvnSession};
pub use commit::{CommitBuilder, CommitStreamBuilder, SvndiffMode};
pub use editor::{
    AsyncEditorEventHandler, EditorCommand, EditorEvent, EditorEventHandler, Report, ReportCommand,
};
pub use error::{ServerError, ServerErrorItem, SvnError};
pub use export::{FsEditor, TokioFsEditor};
pub use pool::{
    PooledSession, SessionPool, SessionPoolConfig, SessionPoolHealthCheck, SessionPoolKey,
    SessionPools,
};
pub use textdelta::{
    RecordedTextDelta, TextDeltaApplier, TextDeltaApplierSync, TextDeltaRecorder, apply_textdelta,
    apply_textdelta_sync,
};
/// Convenience alias for results returned by this crate.
pub type Result<T> = std::result::Result<T, SvnError>;
pub use options::{
    CommitLockToken, CommitOptions, DiffOptions, GetFileOptions, ListOptions, LockManyOptions,
    LockOptions, LockTarget, LogOptions, LogRevProps, ReplayOptions, ReplayRangeOptions,
    StatusOptions, SwitchOptions, UnlockManyOptions, UnlockOptions, UnlockTarget, UpdateOptions,
};
/// Low-level wire-protocol types and helpers.
pub mod raw {
    pub use crate::rasvn::SvnItem;
}
pub use types::{
    BlameLine, Capability, ChangedPath, CommitInfo, Depth, DirEntry, DirListing, DirentField,
    FileRev, FileRevContents, GetFileResult, InheritedProps, LocationEntry, LocationSegment,
    LockDesc, LogEntry, MergeInfoCatalog, MergeInfoInheritance, NodeKind, PropDelta, PropertyList,
    RepositoryInfo, ServerInfo, StatEntry,
};
pub use url::SvnUrl;
