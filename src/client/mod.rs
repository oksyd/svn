use std::fmt::Formatter;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tracing::debug;

use crate::path::{validate_rel_dir_path, validate_rel_path};
use crate::rasvn::conn::{RaSvnConnection, RaSvnConnectionConfig};
use crate::rasvn::edit::{
    EditorDriveStatus, drive_editor, drive_editor_async, encode_editor_command, parse_failure,
    send_report,
};
use crate::rasvn::parse::{
    opt_tuple_wordish, parse_commit_info, parse_file_rev_entry, parse_get_dir_listing,
    parse_get_file_response_params, parse_iproplist, parse_list_dirent, parse_location_entry,
    parse_location_segment, parse_lockdesc, parse_log_entry, parse_mergeinfo_catalog,
    parse_proplist, parse_stat_params,
};
use crate::raw::SvnItem;
use crate::{
    AsyncEditorEventHandler, Capability, CommitInfo, CommitOptions, Depth, DiffOptions, DirEntry,
    DirListing, DirentField, EditorCommand, EditorEvent, EditorEventHandler, GetFileOptions,
    GetFileResult, InheritedProps, ListOptions, LocationEntry, LocationSegment, LockDesc,
    LockManyOptions, LockOptions, LockTarget, LogEntry, LogOptions, LogRevProps, MergeInfoCatalog,
    MergeInfoInheritance, NodeKind, PropertyList, ReplayOptions, ReplayRangeOptions, Report,
    ReportCommand, ServerInfo, StatEntry, StatusOptions, SvnError, SvnUrl, SwitchOptions,
    UnlockManyOptions, UnlockOptions, UnlockTarget, UpdateOptions,
};

mod api;
mod buffer;
mod commit_ops;
mod connect;
mod core;
mod data;
mod history;
mod list;
mod locks;
mod meta;
mod report;
mod session;
#[cfg(test)]
mod tests;

use buffer::LimitedVecWriter;
pub use core::{RaSvnClient, RaSvnSession};
use session::{is_retryable_error, is_unknown_command_error, should_drop_connection};
