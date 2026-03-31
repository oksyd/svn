use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use tokio::io::AsyncWriteExt;

use crate::editor::{
    AsyncEditorEventHandler, EditorEvent, EditorEventHandler, Report, ReportCommand,
};
use crate::options::UpdateOptions;
use crate::path::validate_rel_dir_path_ref;
use crate::textdelta::{TextDeltaApplierFile, TextDeltaApplierFileSync};
use crate::{RaSvnClient, RaSvnSession, SvnError};

mod api;
pub use fs::FsEditor;
mod fs;
mod shared;
mod tokio_fs;
pub use tokio_fs::TokioFsEditor;
#[cfg(test)]
mod tests;
