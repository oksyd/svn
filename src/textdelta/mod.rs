//! Helpers for applying Subversion text deltas (svndiff0/1/2).
//!
//! Many `ra_svn` operations return raw svndiff chunks (for example
//! [`crate::EditorEvent::TextDeltaChunk`]). This module provides a small,
//! streaming decoder that can apply those chunks to a base file and write the
//! resulting bytes to an [`tokio::io::AsyncWrite`].
//!
//! For integration with synchronous consumers (for example an
//! [`crate::EditorEventHandler`] that writes to disk), see
//! [`TextDeltaApplierSync`] / [`apply_textdelta_sync`].

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom, Write};

use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

use crate::SvnError;
use crate::editor::{EditorEvent, EditorEventHandler};

mod applier;
mod decode;
mod record;
#[cfg(test)]
mod tests;

pub use applier::{TextDeltaApplier, TextDeltaApplierSync, apply_textdelta, apply_textdelta_sync};
pub(crate) use applier::{TextDeltaApplierFile, TextDeltaApplierFileSync};
pub use record::{RecordedTextDelta, TextDeltaRecorder};

const SVNDIFF_HEADER_LEN: usize = 4;
const SVNDIFF_HEADER_V0: [u8; 4] = *b"SVN\0";
const SVNDIFF_HEADER_V1: [u8; 4] = *b"SVN\x01";
const SVNDIFF_HEADER_V2: [u8; 4] = *b"SVN\x02";

const MAX_ENCODED_UINT_LEN: usize = 10;
const DELTA_WINDOW_MAX: usize = 64 * 1024;
const MAX_INSTRUCTION_LEN: usize = 2 * MAX_ENCODED_UINT_LEN + 1;
const MAX_INSTRUCTION_SECTION_LEN: usize = DELTA_WINDOW_MAX * MAX_INSTRUCTION_LEN;
