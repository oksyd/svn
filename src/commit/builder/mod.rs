use std::collections::{BTreeMap, BTreeSet};

use crate::path::{validate_rel_dir_path, validate_rel_path};
use crate::svndiff::{SvndiffVersion, encode_fulltext_with_options};
use crate::{Capability, EditorCommand, NodeKind, RaSvnSession, SvnError};

use super::util::{SvndiffMode, TokenGen, dir_prefixes, parent_dir, select_svndiff_version};

mod build;
mod plan;

/// High-level commit editor builder.
///
/// This builder generates a low-level editor command sequence so callers don't
/// need to manually craft `EditorCommand::TextDeltaChunk` values.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
pub struct CommitBuilder {
    base_rev: Option<u64>,
    svndiff: SvndiffMode,
    zlib_level: u32,
    window_size: usize,
    changes: Vec<Change>,
}

#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug)]
enum Change {
    PutFile {
        path: String,
        contents: Vec<u8>,
    },
    MkdirP {
        path: String,
    },
    Delete {
        path: String,
    },
    Copy {
        from_path: String,
        from_rev: Option<u64>,
        to_path: String,
    },
    FileProp {
        path: String,
        name: String,
        value: Option<Vec<u8>>,
    },
    DirProp {
        path: String,
        name: String,
        value: Option<Vec<u8>>,
    },
}

impl CommitBuilder {
    /// Creates an empty commit builder.
    pub fn new() -> Self {
        Self {
            base_rev: None,
            svndiff: SvndiffMode::Auto,
            zlib_level: 5,
            window_size: 64 * 1024,
            changes: Vec::new(),
        }
    }

    /// Sets the base revision used for `open-root` and `open-file`.
    ///
    /// If not set, [`CommitBuilder::build_editor_commands`] will query the
    /// server for `HEAD` via `get-latest-rev`.
    pub fn with_base_rev(mut self, base_rev: u64) -> Self {
        self.base_rev = Some(base_rev);
        self
    }

    /// Sets the svndiff version to use.
    pub fn with_svndiff(mut self, svndiff: SvndiffMode) -> Self {
        self.svndiff = svndiff;
        self
    }

    /// Sets the zlib compression level used by svndiff1.
    ///
    /// Valid values are `0..=9`. `0` disables compression and sends raw data
    /// with an svndiff1 size prefix (matching Subversion behavior).
    pub fn with_zlib_level(mut self, level: u32) -> Self {
        self.zlib_level = level;
        self
    }

    /// Sets the maximum data size per svndiff window.
    pub fn with_window_size(mut self, window_size: usize) -> Self {
        self.window_size = window_size;
        self
    }

    /// Adds or replaces the full contents of `path`.
    ///
    /// If the path does not exist at `base_rev`, it will be added. If it exists
    /// as a file, it will be replaced via a textdelta. Directory paths are
    /// rejected.
    pub fn put_file(mut self, path: impl Into<String>, contents: impl Into<Vec<u8>>) -> Self {
        self.changes.push(Change::PutFile {
            path: path.into(),
            contents: contents.into(),
        });
        self
    }

    /// Ensures `path` exists as a directory, creating parent directories as needed.
    ///
    /// If the directory already exists at `base_rev`, this is a no-op.
    pub fn mkdir_p(mut self, path: impl Into<String>) -> Self {
        self.changes.push(Change::MkdirP { path: path.into() });
        self
    }

    /// Deletes `path` (file or directory).
    pub fn delete(mut self, path: impl Into<String>) -> Self {
        self.changes.push(Change::Delete { path: path.into() });
        self
    }

    /// Copies `from_path@base_rev` to `to_path`.
    pub fn copy(mut self, from_path: impl Into<String>, to_path: impl Into<String>) -> Self {
        self.changes.push(Change::Copy {
            from_path: from_path.into(),
            from_rev: None,
            to_path: to_path.into(),
        });
        self
    }

    /// Copies `from_path@from_rev` to `to_path`.
    pub fn copy_from_rev(
        mut self,
        from_path: impl Into<String>,
        from_rev: u64,
        to_path: impl Into<String>,
    ) -> Self {
        self.changes.push(Change::Copy {
            from_path: from_path.into(),
            from_rev: Some(from_rev),
            to_path: to_path.into(),
        });
        self
    }

    /// Moves `from_path@base_rev` to `to_path`.
    ///
    /// This is expressed as `copy` + `delete`.
    pub fn move_path(self, from_path: impl Into<String>, to_path: impl Into<String>) -> Self {
        let from_path = from_path.into();
        self.copy(from_path.clone(), to_path).delete(from_path)
    }

    /// Moves `from_path@from_rev` to `to_path`.
    ///
    /// This is expressed as `copy_from_rev` + `delete`.
    pub fn move_path_from_rev(
        self,
        from_path: impl Into<String>,
        from_rev: u64,
        to_path: impl Into<String>,
    ) -> Self {
        let from_path = from_path.into();
        self.copy_from_rev(from_path.clone(), from_rev, to_path)
            .delete(from_path)
    }

    /// Sets or deletes a file property.
    pub fn file_prop(
        mut self,
        path: impl Into<String>,
        name: impl Into<String>,
        value: Option<Vec<u8>>,
    ) -> Self {
        self.changes.push(Change::FileProp {
            path: path.into(),
            name: name.into(),
            value,
        });
        self
    }

    /// Sets a file property.
    pub fn set_file_prop(
        self,
        path: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        self.file_prop(path, name, Some(value.into()))
    }

    /// Deletes a file property.
    pub fn delete_file_prop(self, path: impl Into<String>, name: impl Into<String>) -> Self {
        self.file_prop(path, name, None)
    }

    /// Sets or deletes a directory property.
    pub fn dir_prop(
        mut self,
        path: impl Into<String>,
        name: impl Into<String>,
        value: Option<Vec<u8>>,
    ) -> Self {
        self.changes.push(Change::DirProp {
            path: path.into(),
            name: name.into(),
            value,
        });
        self
    }

    /// Sets a directory property.
    pub fn set_dir_prop(
        self,
        path: impl Into<String>,
        name: impl Into<String>,
        value: impl Into<Vec<u8>>,
    ) -> Self {
        self.dir_prop(path, name, Some(value.into()))
    }

    /// Deletes a directory property.
    pub fn delete_dir_prop(self, path: impl Into<String>, name: impl Into<String>) -> Self {
        self.dir_prop(path, name, None)
    }
}

impl Default for CommitBuilder {
    fn default() -> Self {
        Self::new()
    }
}
