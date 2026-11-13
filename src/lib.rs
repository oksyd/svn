//! Async Subversion repository access over `svn://` and optional `svn+ssh://`.
//!
//! Use this crate to read files and history, export directories, manage locks,
//! and commit changes through the `ra_svn` protocol served by `svnserve`.
//! Operations run on Tokio. The crate implements remote repository access;
//! working copy management and HTTP(S) or `file://` transports are outside its scope.
//!
//! Start with [`SvnUrl`], [`RaSvnClient`], and [`RaSvnSession`]. All public API
//! types are available at the crate root, except wire items in [`raw`].
//!
//! # Quick start
//!
//! Add these dependencies to your `Cargo.toml` (Rust 1.100.0 or newer):
//!
//! ```toml
//! [dependencies]
//! svn = "0.1"
//! tokio = { version = "1", features = ["rt"] }
//! ```
//!
//! This example opens an anonymous session and lists its root at a fixed revision.
//! In an existing Tokio application, use the async block's contents directly.
//!
//! ```rust,no_run
//! use svn::{RaSvnClient, SvnUrl};
//!
//! fn main() -> svn::Result<()> {
//!     let runtime = tokio::runtime::Builder::new_current_thread()
//!         .enable_all()
//!         .build()?;
//!
//!     runtime.block_on(async {
//!         let client = RaSvnClient::new(SvnUrl::parse("svn://example.com/repo")?, None, None);
//!         let mut session = client.open_session().await?;
//!         let revision = session.get_latest_rev().await?;
//!
//!         // An empty path selects the session URL; pin the revision for this read.
//!         let listing = session.list_dir("", Some(revision)).await?;
//!         for entry in listing.entries {
//!             println!("{} {}", entry.kind, entry.path);
//!         }
//!         Ok(())
//!     })
//! }
//! ```
//!
//! # Choose an API
//!
//! | Task | Entry point |
//! | --- | --- |
//! | Read a small file into memory | [`RaSvnSession::get_file_bytes`] |
//! | Stream a file to an async writer | [`RaSvnSession::get_file`], [`GetFileOptions`] |
//! | List a directory or subtree | [`RaSvnSession::list_dir`], [`RaSvnSession::list_recursive`] |
//! | Read commit history | [`RaSvnSession::log_with_options`], [`LogOptions`] |
//! | Process history incrementally | [`RaSvnSession::log_each`], [`RaSvnSession::log_each_retrying`] |
//! | Read properties and file history | [`RaSvnSession::proplist`], [`RaSvnSession::get_file_revs_with_contents`] |
//! | Produce a text diff or blame | [`RaSvnSession::diff_file_unified`], [`RaSvnSession::blame_file`] |
//! | Export files to disk | [`RaSvnSession::export_to_dir`] |
//! | Commit in-memory changes or stream uploads | [`CommitBuilder`], [`CommitStreamBuilder`] |
//! | Acquire or release locks | [`RaSvnSession::lock`], [`RaSvnSession::unlock`] |
//! | Run independent requests concurrently | [`SessionPool`], [`SessionPools`] |
//! | Consume protocol edits | [`Report`], [`EditorEvent`], [`AsyncEditorEventHandler`] |
//!
//! # Paths and revisions
//!
//! File and directory request paths are relative to the session's current URL.
//! For a session at `svn://host/repo/trunk`, `"src/lib.rs"` selects
//! `svn://host/repo/trunk/src/lib.rs`. Directory operations accept `""` for the
//! session root; file downloads require a nonempty path. Use `/` separators.
//! Parent traversal (`..`) and NUL bytes are rejected with [`SvnError::InvalidPath`].
//! Returned paths may follow different conventions; consult the result type,
//! especially [`ChangedPath`] and [`DirEntry`].
//!
//! Revisions are repository-wide `u64` numbers. Revision `0` is the initial
//! revision, not an alias for HEAD. Call [`RaSvnSession::get_latest_rev`] to
//! resolve HEAD, then reuse that number for reads that need a consistent
//! snapshot. Optional revisions use each operation's documented server default.
//! [`LogOptions::between`] includes both endpoints and controls traversal order.
//!
//! File contents and [`PropertyList`] values are bytes. Decode them as text only
//! when the content's encoding is known. Download methods require a `max_bytes`
//! limit; exceeding it returns an error rather than a successful truncated result.
//!
//! # Connections and concurrency
//!
//! [`RaSvnClient`] stores configuration and opens connections on demand. Its
//! convenience methods, such as [`RaSvnClient::get_latest_rev`], open a new session
//! for each call. Reuse a [`RaSvnSession`] for sequential operations on one
//! connection, or a [`SessionPool`] for bounded concurrent access. Cloning a
//! client copies configuration; cloning a pool shares the same pool.
//!
//! Default timeouts are 10 seconds for connecting and 60 seconds for protocol
//! reads and writes. They are configurable on [`RaSvnClient`]; read/write timeouts
//! apply to transport I/O rather than the total duration of a multi-step operation.
//! The runtime must have I/O and time enabled, as in the quick start above.
//!
//! For an existing proxy or tunnel, use [`RaSvnClient::open_session_with_stream`].
//! A custom-stream session cannot reconnect itself: supply a new stream and open
//! a new session when the connection is lost.
//!
//! # Authentication and features
//!
//! For `svn://`, pass optional username/password credentials to
//! [`RaSvnClient::new`]. Built-in mechanisms are `ANONYMOUS`, `PLAIN`, and
//! `CRAM-MD5`; the server determines which are available. Plain `svn://` has no
//! transport encryption. Use SSH or a suitably configured SASL security layer
//! when the connection needs protection.
//!
//! No optional features are enabled by default.
//!
//! | Cargo feature | Behavior |
//! | --- | --- |
//! | `ssh` | Enables `svn+ssh://`, using `SshConfig` to authenticate over SSH and run `svnserve -t` remotely. SSH credentials are separate from the `ra_svn` credentials passed to `RaSvnClient::new`. |
//! | `serde` | Adds Serde serialization/deserialization to supported public data types. |
//! | `cyrus-sasl` | Enables additional SASL mechanisms and negotiated security layers. Requires system `libsasl2` and the chosen mechanism's plugins at runtime. |
//!
//! # Errors and retries
//!
//! Operations return [`Result<T>`] with [`SvnError`]. Server failures preserve
//! a [`ServerError`] containing the command context and Subversion error chain.
//! Network and filesystem failures use [`SvnError::Io`]; protocol violations and
//! some local limits use [`SvnError::Protocol`].
//!
//! Retry behavior depends on the operation. [`RaSvnClient::with_reconnect_retries`]
//! controls retries for connection setup and methods that support them.
//! [`RaSvnSession::log_each`] does not resume interrupted delivery; choose
//! [`RaSvnSession::log_each_retrying`] when revision-based deduplication is suitable.
//! Commits are not automatically replayed after a failure. If a connection fails
//! after submission, check repository state before submitting the same edit again.
//!
//! Streaming writers and callbacks can observe partial results before an error;
//! the crate does not roll back their effects. If an in-flight operation is
//! cancelled (for example by dropping its future), reconnect or replace the
//! session before issuing another command. See [`RaSvnSession`] for pool usage.
//!
//! # Reports and editors
//!
//! [`Report`] describes the caller's existing tree and revisions. Operations such
//! as [`RaSvnSession::update`] compare that state with the repository and send
//! [`EditorEvent`] values to a handler. File delta chunks contain svndiff data;
//! use [`TextDeltaApplier`] or [`TextDeltaApplierSync`] to reconstruct contents.
//! [`RaSvnSession::export_to_dir`] supplies the report and filesystem handler for
//! a fresh export. For client-to-server edits, prefer [`CommitBuilder`] over
//! constructing [`EditorCommand`] sequences manually.

#![cfg_attr(docsrs, feature(doc_cfg))]
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
#[cfg(feature = "ssh")]
mod ssh;
mod svndiff;
#[cfg(test)]
mod test_support;
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
#[cfg(feature = "ssh")]
pub use ssh::{SshAuth, SshConfig, SshHostKeyPolicy};
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
