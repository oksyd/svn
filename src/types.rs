//! Public data types returned by this crate.
//!
//! Most of these types are thin wrappers around the values returned by the
//! `ra_svn` protocol.

use std::collections::BTreeMap;
use std::fmt::{Display, Formatter};

/// A Subversion property list (`name -> raw bytes`).
///
/// Property values can be binary; callers should treat the value as opaque
/// bytes unless they know it is UTF-8.
pub type PropertyList = BTreeMap<String, Vec<u8>>;
/// A map of `path -> mergeinfo` strings as returned by `get-mergeinfo`.
pub type MergeInfoCatalog = BTreeMap<String, String>;

/// Inherited properties for a path.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InheritedProps {
    /// The repository path these properties apply to.
    pub path: String,
    /// The inherited property list.
    pub props: PropertyList,
}

/// Repository metadata returned by the server.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryInfo {
    /// Repository UUID.
    pub uuid: String,
    /// Repository root URL.
    ///
    /// Some older servers may not provide a root URL during handshake; in that
    /// case this is an empty string.
    pub root_url: String,
    /// Server-reported repository capabilities.
    pub capabilities: Vec<String>,
}

/// Information negotiated during the initial handshake.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerInfo {
    /// Capability list negotiated during handshake.
    ///
    /// This includes capabilities announced in the initial greeting and any
    /// additional capabilities announced in `repos-info`.
    pub server_caps: Vec<String>,
    /// Repository metadata.
    pub repository: RepositoryInfo,
}

/// A successful commit result returned by [`crate::RaSvnSession::commit`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitInfo {
    /// The new committed revision number.
    pub new_rev: u64,
    /// Commit date, if provided by the server (usually an RFC3339-ish string).
    pub date: Option<String>,
    /// Commit author, if provided by the server.
    pub author: Option<String>,
    /// Server-reported post-commit error, if any.
    pub post_commit_err: Option<String>,
}

/// Result metadata returned by [`crate::RaSvnSession::get_file_with_result`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GetFileResult {
    /// The revision that was actually served.
    pub rev: u64,
    /// Optional checksum string (as reported by the server).
    pub checksum: Option<String>,
    /// File properties (if requested).
    pub props: PropertyList,
    /// Inherited properties (if requested and supported by the server).
    pub inherited_props: Vec<InheritedProps>,
    /// Number of bytes streamed to the provided output writer.
    pub bytes_written: u64,
}

/// A `(revision, path)` pair as returned by `get-locations`.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocationEntry {
    /// The revision number.
    pub rev: u64,
    /// Repository-relative path at this revision.
    pub path: String,
}

/// A location segment as returned by `get-location-segments`.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocationSegment {
    /// Start revision of the segment (inclusive).
    pub range_start: u64,
    /// End revision of the segment (inclusive).
    pub range_end: u64,
    /// Repository path for this segment, or `None` for gaps.
    pub path: Option<String>,
}

/// Controls how mergeinfo may be inherited when requesting mergeinfo.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeInfoInheritance {
    /// Only explicit mergeinfo on the requested paths.
    Explicit,
    /// Include inherited mergeinfo.
    Inherited,
    /// Use the nearest ancestor with mergeinfo.
    NearestAncestor,
}

impl MergeInfoInheritance {
    pub(crate) fn as_word(self) -> &'static str {
        match self {
            Self::Explicit => "explicit",
            Self::Inherited => "inherited",
            Self::NearestAncestor => "nearest-ancestor",
        }
    }
}

/// A single property delta entry (name + new value).
///
/// `value == None` represents deletion of the property.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PropDelta {
    /// Property name.
    pub name: String,
    /// New property value (raw bytes), or `None` to delete.
    pub value: Option<Vec<u8>>,
}

/// A file revision entry as returned by `get-file-revs`.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRev {
    /// Repository-relative path.
    pub path: String,
    /// Revision number.
    pub rev: u64,
    /// Revision properties for this revision.
    pub rev_props: PropertyList,
    /// Property deltas for this revision.
    pub prop_deltas: Vec<PropDelta>,
    /// Whether this is a merged revision.
    pub merged_revision: bool,
    /// Raw delta chunks (svndiff) as received from the server.
    pub delta_chunks: Vec<Vec<u8>>,
}

/// A [`FileRev`] entry with materialized file contents.
///
/// This is useful when you want a `get-file-revs` result that is directly
/// consumable without manually applying svndiff chunks.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileRevContents {
    /// The file-revision metadata and raw delta chunks.
    pub file_rev: FileRev,
    /// The full file contents for this revision.
    pub contents: Vec<u8>,
}

/// One annotated line as returned by [`crate::RaSvnSession::blame_file`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    /// The revision that last changed this line (best-effort, line-based).
    pub rev: u64,
    /// The author for `rev`, if available in revision properties.
    pub author: Option<String>,
    /// The date for `rev`, if available in revision properties.
    pub date: Option<String>,
    /// The line contents (includes the trailing `\\n` when present).
    pub line: String,
}

/// A lock description as returned by `get-lock(s)` or `lock`.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockDesc {
    /// Repository-relative path (no leading `/`) that is locked.
    pub path: String,
    /// Opaque lock token.
    pub token: String,
    /// Lock owner.
    pub owner: String,
    /// Optional lock comment.
    pub comment: Option<String>,
    /// Creation date string as reported by the server.
    pub created: String,
    /// Expiration date string as reported by the server, if any.
    pub expires: Option<String>,
}

/// A log entry returned by `log`.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    /// Revision number.
    pub rev: u64,
    /// Changed paths list (may be empty if not requested).
    pub changed_paths: Vec<ChangedPath>,
    /// Author, if provided.
    pub author: Option<String>,
    /// Date, if provided.
    pub date: Option<String>,
    /// Commit message, if provided.
    pub message: Option<String>,
    /// Revision properties returned by the server.
    ///
    /// This may be empty if not requested or if the server does not support
    /// custom revision properties via `log`.
    pub rev_props: PropertyList,
    /// Whether this log entry has child entries (merged revisions).
    pub has_children: bool,
    /// Whether this entry represents an invalid revision marker.
    ///
    /// Subversion uses this to mark the end of a merged-revision subtree.
    pub invalid_revnum: bool,
    /// Whether this entry represents a subtractive merge.
    pub subtractive_merge: bool,
}

/// A single path change entry within a [`LogEntry`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedPath {
    /// Change action (usually `A`, `D`, `M`, `R`).
    pub action: String,
    /// Changed repository path.
    pub path: String,
    /// Copy source path, if this change was made by a copy.
    pub copy_from_path: Option<String>,
    /// Copy source revision, if this change was made by a copy.
    pub copy_from_rev: Option<u64>,
    /// Node kind, if provided by the server.
    pub node_kind: Option<NodeKind>,
    /// Whether text was modified, if known.
    pub text_mods: Option<bool>,
    /// Whether props were modified, if known.
    pub prop_mods: Option<bool>,
}

/// The kind of a node in the repository.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NodeKind {
    /// No node exists at the requested path/revision.
    None,
    /// A file node.
    File,
    /// A directory node.
    Dir,
    /// An unknown kind (usually a forward-compatibility fallback).
    Unknown,
}

impl NodeKind {
    pub(crate) fn from_word(word: &str) -> Self {
        match word {
            "none" => Self::None,
            "file" => Self::File,
            "dir" => Self::Dir,
            _ => Self::Unknown,
        }
    }

    /// Returns a stable string representation used in the `ra_svn` protocol.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::File => "file",
            Self::Dir => "dir",
            Self::Unknown => "unknown",
        }
    }
}

impl Display for NodeKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// A directory entry as returned by directory listing operations.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirEntry {
    /// Entry name (basename).
    pub name: String,
    /// Entry path relative to the listing root.
    pub path: String,
    /// Node kind.
    pub kind: NodeKind,
    /// File size, if provided.
    pub size: Option<u64>,
    /// Whether the node has properties, if provided.
    pub has_props: Option<bool>,
    /// Created revision, if provided.
    pub created_rev: Option<u64>,
    /// Created date, if provided.
    pub created_date: Option<String>,
    /// Last author, if provided.
    pub last_author: Option<String>,
}

/// Result of `get-dir` as returned by [`crate::RaSvnSession::list_dir`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirListing {
    /// The revision served by the server.
    pub rev: u64,
    /// Directory entries.
    pub entries: Vec<DirEntry>,
}

/// Subversion depth value (used by `list`, `update`, `switch`, `status`, etc.).
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Depth {
    /// Exclude entries (the target itself only).
    Empty,
    /// Include file children.
    Files,
    /// Include immediate children (files and dirs) but not recurse.
    Immediates,
    /// Fully recursive.
    Infinity,
}

impl Depth {
    pub(crate) fn as_word(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Files => "files",
            Self::Immediates => "immediates",
            Self::Infinity => "infinity",
        }
    }
}

/// Fields to request for directory entries when using the `list` capability.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirentField {
    /// Node kind.
    Kind,
    /// File size.
    Size,
    /// Whether properties are present.
    HasProps,
    /// Created revision.
    CreatedRev,
    /// Time / date.
    Time,
    /// Last author.
    LastAuthor,
    /// `word` field (server-specific).
    Word,
}

impl DirentField {
    pub(crate) fn as_word(self) -> &'static str {
        match self {
            Self::Kind => "kind",
            Self::Size => "size",
            Self::HasProps => "has-props",
            Self::CreatedRev => "created-rev",
            Self::Time => "time",
            Self::LastAuthor => "last-author",
            Self::Word => "word",
        }
    }
}

/// A `stat` result entry.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatEntry {
    /// Node kind.
    pub kind: NodeKind,
    /// File size, if provided.
    pub size: Option<u64>,
    /// Whether properties are present, if provided.
    pub has_props: Option<bool>,
    /// Created revision, if provided.
    pub created_rev: Option<u64>,
    /// Created date, if provided.
    pub created_date: Option<String>,
    /// Last author, if provided.
    pub last_author: Option<String>,
}

/// A protocol capability that may be announced during handshake.
///
/// Capabilities are used to gate optional protocol features and commands.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Capability {
    /// The mandatory edit pipelining capability.
    EditPipeline,
    /// Support for svndiff1 deltas.
    Svndiff1,
    /// Support for accepting svndiff2 deltas.
    AcceptsSvndiff2,
    /// Support for `absent-dir` / `absent-file` editor commands.
    AbsentEntries,
    /// Support for setting revision properties during commit (`commit-revprops`).
    CommitRevProps,
    /// Support for the `get-mergeinfo` command.
    MergeInfo,
    /// Support for depth-related parameters.
    Depth,
    /// Support for `change-rev-prop2` (`atomic-revprops`).
    AtomicRevProps,
    /// Support for inherited properties (`inherited-props` / `get-iprops`).
    InheritedProps,
    /// Support for requesting revision properties from `log` (`log-revprops`).
    LogRevProps,
    /// Support for partial replay (`partial-replay`).
    PartialReplay,
    /// Support for ephemeral transaction properties (`ephemeral-txnprops`).
    EphemeralTxnProps,
    /// Support for retrieving file revs in reverse order (`get-file-revs-reverse`).
    GetFileRevsReverse,
    /// Support for the `list` command.
    List,
}

impl Capability {
    /// Returns the wire capability word used by the `ra_svn` protocol.
    pub fn as_wire_word(self) -> &'static str {
        match self {
            Self::EditPipeline => "edit-pipeline",
            Self::Svndiff1 => "svndiff1",
            Self::AcceptsSvndiff2 => "accepts-svndiff2",
            Self::AbsentEntries => "absent-entries",
            Self::CommitRevProps => "commit-revprops",
            Self::MergeInfo => "mergeinfo",
            Self::Depth => "depth",
            Self::AtomicRevProps => "atomic-revprops",
            Self::InheritedProps => "inherited-props",
            Self::LogRevProps => "log-revprops",
            Self::PartialReplay => "partial-replay",
            Self::EphemeralTxnProps => "ephemeral-txnprops",
            Self::GetFileRevsReverse => "get-file-revs-reverse",
            Self::List => "list",
        }
    }
}
