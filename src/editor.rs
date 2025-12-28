//! Types for report and editor flows.

use crate::{Depth, PropertyList, SvnError};

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Client-to-server report commands used by operations like `update`/`switch`.
pub enum ReportCommand {
    /// Adds or updates a path in the report.
    SetPath {
        /// Repository-relative path.
        path: String,
        /// Revision to report for this path.
        rev: u64,
        /// Whether this path should start empty.
        start_empty: bool,
        /// Optional lock token to include.
        lock_token: Option<String>,
        /// Requested depth.
        depth: Depth,
    },
    /// Deletes a path in the report.
    DeletePath {
        /// Repository-relative path.
        path: String,
    },
    /// Links a path to a URL in the report.
    LinkPath {
        /// Repository-relative path.
        path: String,
        /// URL to link to.
        url: String,
        /// Revision to report for this link.
        rev: u64,
        /// Whether this path should start empty.
        start_empty: bool,
        /// Optional lock token to include.
        lock_token: Option<String>,
        /// Requested depth.
        depth: Depth,
    },
    /// Terminates the report successfully.
    FinishReport,
    /// Aborts the report.
    AbortReport,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// A sequence of [`ReportCommand`] values.
pub struct Report {
    /// Commands in the report. Reports must end with `finish-report` or
    /// `abort-report`.
    pub commands: Vec<ReportCommand>,
}

impl Report {
    /// Creates an empty report.
    pub fn new() -> Self {
        Self {
            commands: Vec::new(),
        }
    }

    /// Appends a command to the report.
    pub fn push(&mut self, cmd: ReportCommand) -> &mut Self {
        self.commands.push(cmd);
        self
    }

    /// Appends a `finish-report` terminator.
    pub fn finish(&mut self) -> &mut Self {
        self.commands.push(ReportCommand::FinishReport);
        self
    }
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
/// Server-to-client editor events returned by report-driven operations.
pub enum EditorEvent {
    /// Reports the target revision.
    TargetRev {
        /// Target revision number.
        rev: u64,
    },
    /// Opens the root directory.
    OpenRoot {
        /// Optional base revision.
        rev: Option<u64>,
        /// Root token.
        token: String,
    },
    /// Deletes an entry.
    DeleteEntry {
        /// Repository-relative path.
        path: String,
        /// Revision number.
        rev: u64,
        /// Directory token.
        dir_token: String,
    },
    /// Adds a directory.
    AddDir {
        /// Repository-relative path.
        path: String,
        /// Parent directory token.
        parent_token: String,
        /// Child directory token.
        child_token: String,
        /// Optional copy source `(path, rev)`.
        copy_from: Option<(String, u64)>,
    },
    /// Opens an existing directory.
    OpenDir {
        /// Repository-relative path.
        path: String,
        /// Parent directory token.
        parent_token: String,
        /// Child directory token.
        child_token: String,
        /// Base revision.
        rev: u64,
    },
    /// Changes a directory property.
    ChangeDirProp {
        /// Directory token.
        dir_token: String,
        /// Property name.
        name: String,
        /// Property value (raw bytes), or `None` to delete.
        value: Option<Vec<u8>>,
    },
    /// Closes a directory.
    CloseDir {
        /// Directory token.
        dir_token: String,
    },
    /// Marks a directory as absent.
    AbsentDir {
        /// Repository-relative path.
        path: String,
        /// Parent directory token.
        parent_token: String,
    },
    /// Adds a file.
    AddFile {
        /// Repository-relative path.
        path: String,
        /// Directory token.
        dir_token: String,
        /// File token.
        file_token: String,
        /// Optional copy source `(path, rev)`.
        copy_from: Option<(String, u64)>,
    },
    /// Opens an existing file.
    OpenFile {
        /// Repository-relative path.
        path: String,
        /// Directory token.
        dir_token: String,
        /// File token.
        file_token: String,
        /// Base revision.
        rev: u64,
    },
    /// Begins a text delta stream for a file.
    ApplyTextDelta {
        /// File token.
        file_token: String,
        /// Optional base checksum.
        base_checksum: Option<String>,
    },
    /// A single delta chunk (svndiff).
    TextDeltaChunk {
        /// File token.
        file_token: String,
        /// Raw delta chunk.
        chunk: Vec<u8>,
    },
    /// Marks the end of the delta stream.
    TextDeltaEnd {
        /// File token.
        file_token: String,
    },
    /// Changes a file property.
    ChangeFileProp {
        /// File token.
        file_token: String,
        /// Property name.
        name: String,
        /// Property value (raw bytes), or `None` to delete.
        value: Option<Vec<u8>>,
    },
    /// Closes a file.
    CloseFile {
        /// File token.
        file_token: String,
        /// Optional text checksum.
        text_checksum: Option<String>,
    },
    /// Marks a file as absent.
    AbsentFile {
        /// Repository-relative path.
        path: String,
        /// Parent directory token.
        parent_token: String,
    },
    /// Closes the edit successfully.
    CloseEdit,
    /// Aborts the edit.
    AbortEdit,
    /// Signals the end of a replay stream.
    FinishReplay,
    /// Revision properties sent during replay.
    RevProps {
        /// Revision properties.
        props: PropertyList,
    },
}

/// Client-to-server editor commands for [`crate::RaSvnSession::commit`].
///
/// This is a low-level API: callers must provide a valid sequence of commands
/// and must end with [`EditorCommand::CloseEdit`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorCommand {
    /// Opens the root directory.
    OpenRoot {
        /// Optional base revision.
        rev: Option<u64>,
        /// Root token.
        token: String,
    },
    /// Deletes an entry.
    DeleteEntry {
        /// Repository-relative path.
        path: String,
        /// Revision number.
        rev: u64,
        /// Directory token.
        dir_token: String,
    },
    /// Adds a directory.
    AddDir {
        /// Repository-relative path.
        path: String,
        /// Parent directory token.
        parent_token: String,
        /// Child directory token.
        child_token: String,
        /// Optional copy source `(path, rev)`.
        copy_from: Option<(String, u64)>,
    },
    /// Opens an existing directory.
    OpenDir {
        /// Repository-relative path.
        path: String,
        /// Parent directory token.
        parent_token: String,
        /// Child directory token.
        child_token: String,
        /// Base revision.
        rev: u64,
    },
    /// Changes a directory property.
    ChangeDirProp {
        /// Directory token.
        dir_token: String,
        /// Property name.
        name: String,
        /// Property value (raw bytes), or `None` to delete.
        value: Option<Vec<u8>>,
    },
    /// Closes a directory.
    CloseDir {
        /// Directory token.
        dir_token: String,
    },
    /// Marks a directory as absent.
    AbsentDir {
        /// Repository-relative path.
        path: String,
        /// Parent directory token.
        parent_token: String,
    },
    /// Adds a file.
    AddFile {
        /// Repository-relative path.
        path: String,
        /// Directory token.
        dir_token: String,
        /// File token.
        file_token: String,
        /// Optional copy source `(path, rev)`.
        copy_from: Option<(String, u64)>,
    },
    /// Opens an existing file.
    OpenFile {
        /// Repository-relative path.
        path: String,
        /// Directory token.
        dir_token: String,
        /// File token.
        file_token: String,
        /// Base revision.
        rev: u64,
    },
    /// Begins a text delta stream for a file.
    ApplyTextDelta {
        /// File token.
        file_token: String,
        /// Optional base checksum.
        base_checksum: Option<String>,
    },
    /// A single delta chunk (svndiff).
    TextDeltaChunk {
        /// File token.
        file_token: String,
        /// Raw delta chunk.
        chunk: Vec<u8>,
    },
    /// Marks the end of the delta stream.
    TextDeltaEnd {
        /// File token.
        file_token: String,
    },
    /// Changes a file property.
    ChangeFileProp {
        /// File token.
        file_token: String,
        /// Property name.
        name: String,
        /// Property value (raw bytes), or `None` to delete.
        value: Option<Vec<u8>>,
    },
    /// Closes a file.
    CloseFile {
        /// File token.
        file_token: String,
        /// Optional text checksum.
        text_checksum: Option<String>,
    },
    /// Marks a file as absent.
    AbsentFile {
        /// Repository-relative path.
        path: String,
        /// Parent directory token.
        parent_token: String,
    },
    /// Closes the edit successfully.
    CloseEdit,
    /// Aborts the edit.
    AbortEdit,
}

/// Handler for server-to-client [`EditorEvent`] streams.
pub trait EditorEventHandler {
    /// Called for each incoming editor event.
    fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError>;
}
