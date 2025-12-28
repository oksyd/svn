//! Builder-style option types for higher-level operations.

use crate::{Depth, DirentField, PropertyList};

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::get_file_with_options`].
pub struct GetFileOptions {
    /// Revision to fetch.
    pub rev: u64,
    /// Whether to request file properties.
    pub want_props: bool,
    /// Whether to request inherited properties.
    ///
    /// Note: the standard Subversion client uses a separate `get-iprops` call
    /// instead of setting `want-iprops` in `get-file`, to work around
    /// compatibility issues with some `svnserve` versions.
    pub want_iprops: bool,
    /// Maximum number of bytes to stream.
    pub max_bytes: u64,
}

impl GetFileOptions {
    /// Creates options with no properties requested.
    pub fn new(rev: u64, max_bytes: u64) -> Self {
        Self {
            rev,
            want_props: false,
            want_iprops: false,
            max_bytes,
        }
    }

    /// Requests file properties in the response.
    pub fn with_props(mut self) -> Self {
        self.want_props = true;
        self
    }

    /// Requests inherited properties (if supported by the server).
    pub fn with_iprops(mut self) -> Self {
        self.want_iprops = true;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Which revision properties to request for `log` operations.
pub enum LogRevProps {
    /// Request all revision properties supported by the server.
    All,
    /// Request only a specific set of revision property names.
    Custom(Vec<String>),
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::log_with_options`].
pub struct LogOptions {
    /// Target paths (repository-relative) to include in the log query.
    pub target_paths: Vec<String>,
    /// Start revision (inclusive). `None` uses the server default.
    pub start_rev: Option<u64>,
    /// End revision (inclusive). `None` uses the server default.
    pub end_rev: Option<u64>,
    /// Whether to include changed paths in each log entry.
    pub changed_paths: bool,
    /// Whether to require the target path to exist at the requested revisions.
    pub strict_node: bool,
    /// Maximum number of entries to return (`0` means unlimited).
    pub limit: u64,
    /// Whether to include merged revisions.
    pub include_merged_revisions: bool,
    /// Which revision properties to request.
    pub revprops: LogRevProps,
}

impl Default for LogOptions {
    fn default() -> Self {
        Self {
            target_paths: Vec::new(),
            start_rev: None,
            end_rev: None,
            changed_paths: true,
            strict_node: true,
            limit: 0,
            include_merged_revisions: false,
            revprops: LogRevProps::All,
        }
    }
}

impl LogOptions {
    /// Convenience constructor for a revision range.
    pub fn between(start_rev: u64, end_rev: u64) -> Self {
        Self {
            start_rev: Some(start_rev),
            end_rev: Some(end_rev),
            ..Self::default()
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::list_with_options`].
pub struct ListOptions {
    /// Repository path to list (directory).
    pub path: String,
    /// Revision to list at; `None` uses the server default (usually HEAD).
    pub rev: Option<u64>,
    /// Listing depth.
    pub depth: Depth,
    /// Optional fields to request (server must support the `list` capability).
    pub fields: Vec<DirentField>,
    /// Optional glob patterns to filter entries (server must support `list`).
    pub patterns: Vec<String>,
}

impl ListOptions {
    /// Creates list options for a path at a given depth.
    pub fn new(path: impl Into<String>, depth: Depth) -> Self {
        Self {
            path: path.into(),
            rev: None,
            depth,
            fields: Vec::new(),
            patterns: Vec::new(),
        }
    }

    /// Sets the revision to list at.
    pub fn with_rev(mut self, rev: u64) -> Self {
        self.rev = Some(rev);
        self
    }

    /// Requests additional entry fields from the server.
    pub fn with_fields(mut self, fields: Vec<DirentField>) -> Self {
        self.fields = fields;
        self
    }

    /// Adds glob patterns to filter entries on the server side.
    pub fn with_patterns(mut self, patterns: Vec<String>) -> Self {
        self.patterns = patterns;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::update`].
pub struct UpdateOptions {
    /// Revision to update to; `None` usually means HEAD.
    pub rev: Option<u64>,
    /// Update target path (repository-relative).
    pub target: String,
    /// Update depth.
    pub depth: Depth,
    /// Whether to request copyfrom arguments from the server.
    pub send_copyfrom_args: bool,
    /// Whether to ignore ancestry when applying the report.
    pub ignore_ancestry: bool,
}

impl UpdateOptions {
    /// Creates update options for a target and depth.
    pub fn new(target: impl Into<String>, depth: Depth) -> Self {
        Self {
            rev: None,
            target: target.into(),
            depth,
            send_copyfrom_args: true,
            ignore_ancestry: false,
        }
    }

    /// Sets the revision to update to.
    pub fn with_rev(mut self, rev: u64) -> Self {
        self.rev = Some(rev);
        self
    }

    /// Disables copyfrom arguments (for compatibility with older servers).
    pub fn without_copyfrom_args(mut self) -> Self {
        self.send_copyfrom_args = false;
        self
    }

    /// Ignores ancestry when applying the update.
    pub fn ignore_ancestry(mut self) -> Self {
        self.ignore_ancestry = true;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::switch`].
pub struct SwitchOptions {
    /// Revision to switch to; `None` usually means HEAD.
    pub rev: Option<u64>,
    /// Working target path (repository-relative).
    pub target: String,
    /// URL to switch to.
    pub switch_url: String,
    /// Switch depth.
    pub depth: Depth,
    /// Whether to request copyfrom arguments from the server.
    pub send_copyfrom_args: bool,
    /// Whether to ignore ancestry when applying the report.
    pub ignore_ancestry: bool,
}

impl SwitchOptions {
    /// Creates switch options for a target, URL, and depth.
    pub fn new(target: impl Into<String>, switch_url: impl Into<String>, depth: Depth) -> Self {
        Self {
            rev: None,
            target: target.into(),
            switch_url: switch_url.into(),
            depth,
            send_copyfrom_args: true,
            ignore_ancestry: false,
        }
    }

    /// Sets the revision to switch to.
    pub fn with_rev(mut self, rev: u64) -> Self {
        self.rev = Some(rev);
        self
    }

    /// Disables copyfrom arguments (for compatibility with older servers).
    pub fn without_copyfrom_args(mut self) -> Self {
        self.send_copyfrom_args = false;
        self
    }

    /// Ignores ancestry when applying the switch.
    pub fn ignore_ancestry(mut self) -> Self {
        self.ignore_ancestry = true;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::status`].
pub struct StatusOptions {
    /// Target path (repository-relative).
    pub target: String,
    /// Revision to compare against; `None` usually means HEAD.
    pub rev: Option<u64>,
    /// Status depth.
    pub depth: Depth,
}

impl StatusOptions {
    /// Creates status options for a target and depth.
    pub fn new(target: impl Into<String>, depth: Depth) -> Self {
        Self {
            target: target.into(),
            rev: None,
            depth,
        }
    }

    /// Sets the revision to compare against.
    pub fn with_rev(mut self, rev: u64) -> Self {
        self.rev = Some(rev);
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::diff`].
pub struct DiffOptions {
    /// Revision to diff against; `None` usually means HEAD.
    pub rev: Option<u64>,
    /// Target path (repository-relative).
    pub target: String,
    /// Whether to ignore ancestry.
    pub ignore_ancestry: bool,
    /// URL to diff against.
    pub versus_url: String,
    /// Whether to request text deltas.
    pub text_deltas: bool,
    /// Diff depth.
    pub depth: Depth,
}

impl DiffOptions {
    /// Creates diff options for a target, versus URL, and depth.
    pub fn new(target: impl Into<String>, versus_url: impl Into<String>, depth: Depth) -> Self {
        Self {
            rev: None,
            target: target.into(),
            ignore_ancestry: false,
            versus_url: versus_url.into(),
            text_deltas: true,
            depth,
        }
    }

    /// Sets the revision to diff against.
    pub fn with_rev(mut self, rev: u64) -> Self {
        self.rev = Some(rev);
        self
    }

    /// Ignores ancestry when producing the diff.
    pub fn ignore_ancestry(mut self) -> Self {
        self.ignore_ancestry = true;
        self
    }

    /// Enables or disables requesting text deltas.
    pub fn with_text_deltas(mut self, text_deltas: bool) -> Self {
        self.text_deltas = text_deltas;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::replay`].
pub struct ReplayOptions {
    /// Revision to replay.
    pub revision: u64,
    /// Low water mark revision.
    pub low_water_mark: u64,
    /// Whether to send deltas.
    pub send_deltas: bool,
}

impl ReplayOptions {
    /// Creates replay options for a single revision.
    pub fn new(revision: u64) -> Self {
        Self {
            revision,
            low_water_mark: 0,
            send_deltas: true,
        }
    }

    /// Sets the low water mark.
    pub fn with_low_water_mark(mut self, low_water_mark: u64) -> Self {
        self.low_water_mark = low_water_mark;
        self
    }

    /// Sets whether to request deltas.
    pub fn with_send_deltas(mut self, send_deltas: bool) -> Self {
        self.send_deltas = send_deltas;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::replay_range`].
pub struct ReplayRangeOptions {
    /// Start revision (inclusive).
    pub start_rev: u64,
    /// End revision (inclusive).
    pub end_rev: u64,
    /// Low water mark revision.
    pub low_water_mark: u64,
    /// Whether to send deltas.
    pub send_deltas: bool,
}

impl ReplayRangeOptions {
    /// Creates replay-range options for a revision range.
    pub fn new(start_rev: u64, end_rev: u64) -> Self {
        Self {
            start_rev,
            end_rev,
            low_water_mark: 0,
            send_deltas: true,
        }
    }

    /// Sets the low water mark.
    pub fn with_low_water_mark(mut self, low_water_mark: u64) -> Self {
        self.low_water_mark = low_water_mark;
        self
    }

    /// Sets whether to request deltas.
    pub fn with_send_deltas(mut self, send_deltas: bool) -> Self {
        self.send_deltas = send_deltas;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
/// Options for [`crate::RaSvnSession::lock`].
pub struct LockOptions {
    /// Optional lock comment.
    pub comment: Option<String>,
    /// Whether to steal an existing lock.
    pub steal_lock: bool,
    /// Optional revision the lock is expected to apply to.
    pub current_rev: Option<u64>,
}

impl LockOptions {
    /// Creates default lock options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a lock comment.
    pub fn with_comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    /// Enables stealing an existing lock.
    pub fn steal_lock(mut self) -> Self {
        self.steal_lock = true;
        self
    }

    /// Sets a current revision constraint for the lock request.
    pub fn with_current_rev(mut self, current_rev: u64) -> Self {
        self.current_rev = Some(current_rev);
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
/// Options for [`crate::RaSvnSession::lock_many`].
pub struct LockManyOptions {
    /// Optional lock comment.
    pub comment: Option<String>,
    /// Whether to steal existing locks.
    pub steal_lock: bool,
}

impl LockManyOptions {
    /// Creates default lock-many options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a lock comment.
    pub fn with_comment(mut self, comment: impl Into<String>) -> Self {
        self.comment = Some(comment.into());
        self
    }

    /// Enables stealing existing locks.
    pub fn steal_lock(mut self) -> Self {
        self.steal_lock = true;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// A lock target for [`crate::RaSvnSession::lock_many`].
pub struct LockTarget {
    /// Repository path to lock.
    pub path: String,
    /// Optional current revision constraint.
    pub current_rev: Option<u64>,
}

impl LockTarget {
    /// Creates a lock target for a path.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            current_rev: None,
        }
    }

    /// Sets a current revision constraint for this target.
    pub fn with_current_rev(mut self, current_rev: u64) -> Self {
        self.current_rev = Some(current_rev);
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
/// Options for [`crate::RaSvnSession::unlock`].
pub struct UnlockOptions {
    /// Optional lock token.
    pub token: Option<String>,
    /// Whether to break the lock (force unlock).
    pub break_lock: bool,
}

impl UnlockOptions {
    /// Creates default unlock options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the lock token.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Enables breaking the lock (force unlock).
    pub fn break_lock(mut self) -> Self {
        self.break_lock = true;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq, Default)]
/// Options for [`crate::RaSvnSession::unlock_many`].
pub struct UnlockManyOptions {
    /// Whether to break locks (force unlock).
    pub break_lock: bool,
}

impl UnlockManyOptions {
    /// Creates default unlock-many options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables breaking locks (force unlock).
    pub fn break_lock(mut self) -> Self {
        self.break_lock = true;
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// An unlock target for [`crate::RaSvnSession::unlock_many`].
pub struct UnlockTarget {
    /// Repository path to unlock.
    pub path: String,
    /// Optional lock token.
    pub token: Option<String>,
}

impl UnlockTarget {
    /// Creates an unlock target for a path.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            token: None,
        }
    }

    /// Sets the lock token for this target.
    pub fn with_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// A path/token pair to include in [`crate::CommitOptions::lock_tokens`].
pub struct CommitLockToken {
    /// Locked repository path.
    pub path: String,
    /// Lock token to present during commit.
    pub token: String,
}

impl CommitLockToken {
    /// Creates a path/token pair.
    pub fn new(path: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            token: token.into(),
        }
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Options for [`crate::RaSvnSession::commit`].
///
/// `rev_props` is for additional revision properties; `svn:log` is always
/// derived from `log_message`.
pub struct CommitOptions {
    /// Commit log message (used to set `svn:log`).
    pub log_message: String,
    /// Lock tokens to present during commit.
    pub lock_tokens: Vec<CommitLockToken>,
    /// Whether to keep locks after a successful commit.
    pub keep_locks: bool,
    /// Additional revision properties to set during commit.
    pub rev_props: PropertyList,
}

impl CommitOptions {
    /// Creates commit options with a required log message.
    pub fn new(log_message: impl Into<String>) -> Self {
        Self {
            log_message: log_message.into(),
            lock_tokens: Vec::new(),
            keep_locks: false,
            rev_props: PropertyList::new(),
        }
    }

    /// Sets lock tokens to be included in the commit.
    pub fn with_lock_tokens(mut self, lock_tokens: Vec<CommitLockToken>) -> Self {
        self.lock_tokens = lock_tokens;
        self
    }

    /// Requests that locks be kept after the commit.
    pub fn keep_locks(mut self) -> Self {
        self.keep_locks = true;
        self
    }

    /// Sets additional revision properties.
    pub fn with_rev_props(mut self, rev_props: PropertyList) -> Self {
        self.rev_props = rev_props;
        self
    }
}
