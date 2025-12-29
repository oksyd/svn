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

const SVNDIFF_HEADER_LEN: usize = 4;
const SVNDIFF_HEADER_V0: [u8; 4] = *b"SVN\0";
const SVNDIFF_HEADER_V1: [u8; 4] = *b"SVN\x01";
const SVNDIFF_HEADER_V2: [u8; 4] = *b"SVN\x02";

const MAX_ENCODED_UINT_LEN: usize = 10;
const DELTA_WINDOW_MAX: usize = 64 * 1024;
const MAX_INSTRUCTION_LEN: usize = 2 * MAX_ENCODED_UINT_LEN + 1;
const MAX_INSTRUCTION_SECTION_LEN: usize = DELTA_WINDOW_MAX * MAX_INSTRUCTION_LEN;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SvndiffVersion {
    V0,
    V1,
    V2,
}

impl SvndiffVersion {
    fn from_header(header: &[u8; 4]) -> Option<Self> {
        if header == &SVNDIFF_HEADER_V0 {
            Some(Self::V0)
        } else if header == &SVNDIFF_HEADER_V1 {
            Some(Self::V1)
        } else if header == &SVNDIFF_HEADER_V2 {
            Some(Self::V2)
        } else {
            None
        }
    }
}

#[derive(Debug)]
struct WindowHeader {
    sview_offset: u64,
    sview_len: usize,
    tview_len: usize,
    ins_len: usize,
    new_len: usize,
    header_len: usize,
}

type ParsedWindow = (SvndiffVersion, WindowHeader, Vec<u8>, Vec<u8>);

#[derive(Debug, Default)]
struct CursorBuf {
    buf: Vec<u8>,
    start: usize,
}

impl CursorBuf {
    fn available(&self) -> &[u8] {
        &self.buf[self.start..]
    }

    fn push(&mut self, bytes: &[u8]) {
        if !bytes.is_empty() {
            self.buf.extend_from_slice(bytes);
        }
    }

    fn consume(&mut self, n: usize) {
        self.start = self.start.saturating_add(n);
        if self.start >= self.buf.len() {
            self.buf.clear();
            self.start = 0;
            return;
        }

        // Periodically compact to avoid unbounded growth if we consume in small chunks.
        if self.start > 4096 && self.start * 2 > self.buf.len() {
            self.buf.drain(..self.start);
            self.start = 0;
        }
    }
}

#[derive(Debug, Default)]
struct SvndiffStream {
    any_input: bool,
    header: [u8; SVNDIFF_HEADER_LEN],
    header_bytes: usize,
    version: Option<SvndiffVersion>,
    buf: CursorBuf,
    pending_window: Option<WindowHeader>,
    last_sview_offset: u64,
    last_sview_len: u64,
}

impl SvndiffStream {
    fn push(&mut self, chunk: &[u8]) -> Result<(), SvnError> {
        if chunk.is_empty() {
            return Ok(());
        }
        self.any_input = true;

        let mut input = chunk;
        if self.header_bytes < SVNDIFF_HEADER_LEN {
            let needed = SVNDIFF_HEADER_LEN - self.header_bytes;
            let take = needed.min(input.len());
            self.header[self.header_bytes..self.header_bytes + take]
                .copy_from_slice(&input[..take]);
            self.header_bytes += take;
            input = &input[take..];

            if self.header_bytes == SVNDIFF_HEADER_LEN {
                self.version = SvndiffVersion::from_header(&self.header);
                if self.version.is_none() {
                    return Err(SvnError::Protocol("svndiff has invalid header".into()));
                }
            }
        }

        if !input.is_empty() {
            self.buf.push(input);
        }
        Ok(())
    }

    fn is_identity(&self) -> bool {
        !self.any_input
    }

    fn next_window(&mut self) -> Result<Option<ParsedWindow>, SvnError> {
        let Some(version) = self.version else {
            return Ok(None);
        };

        if self.pending_window.is_none() {
            let avail = self.buf.available();
            let Some(window) = try_parse_window_header(avail)? else {
                if avail.len() > 5 * MAX_ENCODED_UINT_LEN {
                    return Err(SvnError::Protocol(
                        "svndiff contains a too-large window header".into(),
                    ));
                }
                return Ok(None);
            };
            self.pending_window = Some(window);
        }

        let window = self
            .pending_window
            .as_ref()
            .ok_or_else(|| SvnError::Protocol("missing pending window header".into()))?;
        let avail = self.buf.available();
        let needed = window
            .header_len
            .checked_add(window.ins_len)
            .and_then(|n| n.checked_add(window.new_len))
            .ok_or_else(|| SvnError::Protocol("svndiff window size overflow".into()))?;

        if avail.len() < needed {
            return Ok(None);
        }

        let window = self
            .pending_window
            .take()
            .ok_or_else(|| SvnError::Protocol("missing pending window header".into()))?;

        let base = window.header_len;
        let ins_wire = avail[base..base + window.ins_len].to_vec();
        let new_wire =
            avail[base + window.ins_len..base + window.ins_len + window.new_len].to_vec();

        self.buf.consume(needed);
        self.pending_window = None;

        // Backward-sliding source view check (matches Subversion's parser).
        if window.sview_len > 0 {
            let end = window
                .sview_offset
                .checked_add(window.sview_len as u64)
                .ok_or_else(|| SvnError::Protocol("svndiff source view overflow".into()))?;
            let last_end = self
                .last_sview_offset
                .checked_add(self.last_sview_len)
                .ok_or_else(|| SvnError::Protocol("svndiff last source view overflow".into()))?;

            if window.sview_offset < self.last_sview_offset || end < last_end {
                return Err(SvnError::Protocol(
                    "svndiff has backwards-sliding source views".into(),
                ));
            }
        }
        self.last_sview_offset = window.sview_offset;
        self.last_sview_len = window.sview_len as u64;

        Ok(Some((version, window, ins_wire, new_wire)))
    }

    fn finish(&self) -> Result<(), SvnError> {
        if self.is_identity() {
            return Ok(());
        }

        if self.header_bytes < SVNDIFF_HEADER_LEN {
            return Err(SvnError::Protocol(
                "unexpected end of svndiff input (missing header)".into(),
            ));
        }

        if self.pending_window.is_some() || !self.buf.available().is_empty() {
            return Err(SvnError::Protocol(
                "unexpected end of svndiff input (truncated window)".into(),
            ));
        }

        Ok(())
    }
}

fn try_parse_window_header(input: &[u8]) -> Result<Option<WindowHeader>, SvnError> {
    let mut cursor = input;
    let mut header_len = 0usize;

    let Some((sview_offset, used)) = try_decode_uint(cursor)? else {
        return Ok(None);
    };
    cursor = &cursor[used..];
    header_len += used;

    let Some((sview_len_u64, used)) = try_decode_uint(cursor)? else {
        return Ok(None);
    };
    cursor = &cursor[used..];
    header_len += used;

    let Some((tview_len_u64, used)) = try_decode_uint(cursor)? else {
        return Ok(None);
    };
    cursor = &cursor[used..];
    header_len += used;

    let Some((ins_len_u64, used)) = try_decode_uint(cursor)? else {
        return Ok(None);
    };
    cursor = &cursor[used..];
    header_len += used;

    let Some((new_len_u64, used)) = try_decode_uint(cursor)? else {
        return Ok(None);
    };
    header_len += used;

    let sview_len = usize::try_from(sview_len_u64)
        .map_err(|_| SvnError::Protocol("svndiff sview_len overflows usize".into()))?;
    let tview_len = usize::try_from(tview_len_u64)
        .map_err(|_| SvnError::Protocol("svndiff tview_len overflows usize".into()))?;
    let ins_len = usize::try_from(ins_len_u64)
        .map_err(|_| SvnError::Protocol("svndiff ins_len overflows usize".into()))?;
    let new_len = usize::try_from(new_len_u64)
        .map_err(|_| SvnError::Protocol("svndiff new_len overflows usize".into()))?;

    if tview_len > DELTA_WINDOW_MAX
        || sview_len > DELTA_WINDOW_MAX
        || new_len > DELTA_WINDOW_MAX + MAX_ENCODED_UINT_LEN
        || ins_len > MAX_INSTRUCTION_SECTION_LEN
    {
        return Err(SvnError::Protocol(
            "svndiff contains a too-large window".into(),
        ));
    }

    // Check for integer overflow similar to Subversion's parser.
    if ins_len.checked_add(new_len).is_none() {
        return Err(SvnError::Protocol(
            "svndiff contains corrupt window header".into(),
        ));
    }
    if sview_offset.checked_add(sview_len as u64).is_none() {
        return Err(SvnError::Protocol(
            "svndiff contains corrupt window header".into(),
        ));
    }
    if sview_len.checked_add(tview_len).is_none() {
        return Err(SvnError::Protocol(
            "svndiff contains corrupt window header".into(),
        ));
    }

    Ok(Some(WindowHeader {
        sview_offset,
        sview_len,
        tview_len,
        ins_len,
        new_len,
        header_len,
    }))
}

fn try_decode_uint(input: &[u8]) -> Result<Option<(u64, usize)>, SvnError> {
    let mut val: u64 = 0;
    for (idx, &b) in input.iter().enumerate() {
        val = val
            .checked_shl(7)
            .and_then(|v| v.checked_add(u64::from(b & 0x7f)))
            .ok_or_else(|| SvnError::Protocol("svndiff integer overflow".into()))?;
        if (b & 0x80) == 0 {
            return Ok(Some((val, idx + 1)));
        }
    }
    Ok(None)
}

fn decode_section(version: SvndiffVersion, wire: &[u8], limit: usize) -> Result<Vec<u8>, SvnError> {
    match version {
        SvndiffVersion::V0 => Ok(wire.to_vec()),
        SvndiffVersion::V1 => decode_zlib_section(wire, limit),
        SvndiffVersion::V2 => decode_lz4_section(wire, limit),
    }
}

fn decode_zlib_section(wire: &[u8], limit: usize) -> Result<Vec<u8>, SvnError> {
    let Some((orig_len_u64, used)) = try_decode_uint(wire)? else {
        return Err(SvnError::Protocol(
            "svndiff zlib section missing size".into(),
        ));
    };
    let orig_len = usize::try_from(orig_len_u64)
        .map_err(|_| SvnError::Protocol("svndiff zlib size overflows usize".into()))?;
    if orig_len > limit {
        return Err(SvnError::Protocol(
            "svndiff zlib section size too large".into(),
        ));
    }

    let data = &wire[used..];
    if data.len() == orig_len {
        return Ok(data.to_vec());
    }

    let mut decoder = flate2::read::ZlibDecoder::new(data);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|err| SvnError::Protocol(format!("svndiff zlib decode failed: {err}")))?;
    if out.len() != orig_len {
        return Err(SvnError::Protocol(
            "svndiff zlib decoded length mismatch".into(),
        ));
    }
    Ok(out)
}

fn decode_lz4_section(wire: &[u8], limit: usize) -> Result<Vec<u8>, SvnError> {
    let Some((orig_len_u64, used)) = try_decode_uint(wire)? else {
        return Err(SvnError::Protocol(
            "svndiff lz4 section missing size".into(),
        ));
    };
    let orig_len = usize::try_from(orig_len_u64)
        .map_err(|_| SvnError::Protocol("svndiff lz4 size overflows usize".into()))?;
    if orig_len > limit {
        return Err(SvnError::Protocol(
            "svndiff lz4 section size too large".into(),
        ));
    }

    let data = &wire[used..];
    if data.len() == orig_len {
        return Ok(data.to_vec());
    }

    let out = lz4_flex::decompress(data, orig_len)
        .map_err(|err| SvnError::Protocol(format!("svndiff lz4 decode failed: {err}")))?;
    if out.len() != orig_len {
        return Err(SvnError::Protocol(
            "svndiff lz4 decoded length mismatch".into(),
        ));
    }
    Ok(out)
}

fn apply_window(
    base: &[u8],
    window: &WindowHeader,
    instructions: &[u8],
    new_data: &[u8],
) -> Result<Vec<u8>, SvnError> {
    let sview_offset = usize::try_from(window.sview_offset)
        .map_err(|_| SvnError::Protocol("svndiff source view offset overflows usize".into()))?;
    let sview_end = sview_offset
        .checked_add(window.sview_len)
        .ok_or_else(|| SvnError::Protocol("svndiff source view end overflow".into()))?;
    if sview_end > base.len() {
        return Err(SvnError::Protocol(
            "svndiff source view out of bounds for base".into(),
        ));
    }
    let source_view = &base[sview_offset..sview_end];

    apply_window_source(source_view, window.tview_len, instructions, new_data)
}

fn apply_window_source(
    source_view: &[u8],
    tview_len: usize,
    instructions: &[u8],
    new_data: &[u8],
) -> Result<Vec<u8>, SvnError> {
    let mut target = Vec::with_capacity(tview_len);
    let mut ipos = 0usize;
    let mut npos = 0usize;

    while ipos < instructions.len() {
        let selector = instructions[ipos];
        ipos += 1;

        let action = (selector >> 6) & 0x3;
        if action >= 0x3 {
            return Err(SvnError::Protocol("svndiff invalid action".into()));
        }

        let mut len = usize::from(selector & 0x3f);
        if len == 0 {
            let Some((v, used)) = try_decode_uint(&instructions[ipos..])? else {
                return Err(SvnError::Protocol(
                    "svndiff instruction truncated length".into(),
                ));
            };
            ipos += used;
            len = usize::try_from(v)
                .map_err(|_| SvnError::Protocol("svndiff length overflows usize".into()))?;
        }
        if len == 0 {
            return Err(SvnError::Protocol(
                "svndiff instruction has length zero".into(),
            ));
        }

        if target
            .len()
            .checked_add(len)
            .ok_or_else(|| SvnError::Protocol("svndiff target size overflow".into()))?
            > tview_len
        {
            return Err(SvnError::Protocol(
                "svndiff instruction overflows target view".into(),
            ));
        }

        match action {
            0 => {
                let Some((off, used)) = try_decode_uint(&instructions[ipos..])? else {
                    return Err(SvnError::Protocol(
                        "svndiff source instruction missing offset".into(),
                    ));
                };
                ipos += used;
                let off = usize::try_from(off)
                    .map_err(|_| SvnError::Protocol("svndiff offset overflows usize".into()))?;
                if off > source_view.len()
                    || off.checked_add(len).is_none()
                    || off + len > source_view.len()
                {
                    return Err(SvnError::Protocol(
                        "svndiff [src] instruction overflows source view".into(),
                    ));
                }
                target.extend_from_slice(&source_view[off..][..len]);
            }
            1 => {
                let Some((off, used)) = try_decode_uint(&instructions[ipos..])? else {
                    return Err(SvnError::Protocol(
                        "svndiff target instruction missing offset".into(),
                    ));
                };
                ipos += used;
                let off = usize::try_from(off)
                    .map_err(|_| SvnError::Protocol("svndiff offset overflows usize".into()))?;
                let tpos = target.len();
                if off >= tpos {
                    return Err(SvnError::Protocol(
                        "svndiff [tgt] instruction starts beyond target view position".into(),
                    ));
                }

                // Copy, allowing overlap (LZ-style backrefs).
                for i in 0..len {
                    let b = target[off + i];
                    target.push(b);
                }
            }
            2 => {
                if npos.checked_add(len).is_none() || npos + len > new_data.len() {
                    return Err(SvnError::Protocol(
                        "svndiff [new] instruction overflows new data section".into(),
                    ));
                }
                target.extend_from_slice(&new_data[npos..npos + len]);
                npos += len;
            }
            _ => return Err(SvnError::Protocol("svndiff invalid action".into())),
        }
    }

    if target.len() != tview_len {
        return Err(SvnError::Protocol(
            "svndiff delta does not fill the target window".into(),
        ));
    }
    if npos != new_data.len() {
        return Err(SvnError::Protocol(
            "svndiff delta does not contain enough new data".into(),
        ));
    }
    Ok(target)
}

/// Incrementally applies an svndiff textdelta to a base file.
///
/// `push()` accepts the raw svndiff byte chunks as provided by
/// [`crate::EditorEvent::TextDeltaChunk`].
///
/// If the delta stream is empty (no chunks), `finish()` writes `base` unchanged.
pub struct TextDeltaApplier<'a> {
    base: &'a [u8],
    stream: SvndiffStream,
}

impl<'a> TextDeltaApplier<'a> {
    /// Creates a new applier for `base`.
    pub fn new(base: &'a [u8]) -> Self {
        Self {
            base,
            stream: SvndiffStream::default(),
        }
    }

    /// Feeds one raw svndiff chunk and writes completed output windows to `out`.
    pub async fn push<W: AsyncWrite + Unpin>(
        &mut self,
        chunk: &[u8],
        out: &mut W,
    ) -> Result<(), SvnError> {
        self.stream.push(chunk)?;
        while let Some((version, window, ins_wire, new_wire)) = self.stream.next_window()? {
            let instructions = decode_section(version, &ins_wire, MAX_INSTRUCTION_SECTION_LEN)?;
            let new_data = decode_section(version, &new_wire, DELTA_WINDOW_MAX)?;
            let data = apply_window(self.base, &window, &instructions, &new_data)?;
            out.write_all(&data).await?;
        }
        Ok(())
    }

    /// Finishes the delta stream.
    pub async fn finish<W: AsyncWrite + Unpin>(self, out: &mut W) -> Result<(), SvnError> {
        if self.stream.is_identity() {
            out.write_all(self.base).await?;
            return Ok(());
        }
        self.stream.finish()
    }
}

/// Applies an svndiff textdelta (svndiff0/1/2) to `base` and writes the result to `out`.
///
/// This is a convenience wrapper around [`TextDeltaApplier`].
pub async fn apply_textdelta<W, I, B>(base: &[u8], chunks: I, out: &mut W) -> Result<(), SvnError>
where
    W: AsyncWrite + Unpin,
    I: IntoIterator<Item = B>,
    B: AsRef<[u8]>,
{
    let mut applier = TextDeltaApplier::new(base);
    for chunk in chunks {
        applier.push(chunk.as_ref(), out).await?;
    }
    applier.finish(out).await
}

/// Incrementally applies an svndiff textdelta to a base file, writing to a synchronous
/// [`std::io::Write`].
///
/// This is useful for consumers that can't `.await` inside the callback, such as
/// [`crate::EditorEventHandler`] implementations.
pub struct TextDeltaApplierSync<'a> {
    base: &'a [u8],
    stream: SvndiffStream,
}

impl<'a> TextDeltaApplierSync<'a> {
    /// Creates a new applier for `base`.
    pub fn new(base: &'a [u8]) -> Self {
        Self {
            base,
            stream: SvndiffStream::default(),
        }
    }

    /// Feeds one raw svndiff chunk and writes completed output windows to `out`.
    pub fn push<W: Write>(&mut self, chunk: &[u8], out: &mut W) -> Result<(), SvnError> {
        self.stream.push(chunk)?;
        while let Some((version, window, ins_wire, new_wire)) = self.stream.next_window()? {
            let instructions = decode_section(version, &ins_wire, MAX_INSTRUCTION_SECTION_LEN)?;
            let new_data = decode_section(version, &new_wire, DELTA_WINDOW_MAX)?;
            let data = apply_window(self.base, &window, &instructions, &new_data)?;
            out.write_all(&data)?;
        }
        Ok(())
    }

    /// Finishes the delta stream.
    pub fn finish<W: Write>(self, out: &mut W) -> Result<(), SvnError> {
        if self.stream.is_identity() {
            out.write_all(self.base)?;
            return Ok(());
        }
        self.stream.finish()
    }
}

/// Applies an svndiff textdelta (svndiff0/1/2) to `base` and writes the result to `out`.
///
/// This is a convenience wrapper around [`TextDeltaApplierSync`].
pub fn apply_textdelta_sync<W, I, B>(base: &[u8], chunks: I, out: &mut W) -> Result<(), SvnError>
where
    W: Write,
    I: IntoIterator<Item = B>,
    B: AsRef<[u8]>,
{
    let mut applier = TextDeltaApplierSync::new(base);
    for chunk in chunks {
        applier.push(chunk.as_ref(), out)?;
    }
    applier.finish(out)
}

#[derive(Debug)]
pub(crate) struct TextDeltaApplierFileSync {
    base: Option<std::fs::File>,
    base_len: u64,
    stream: SvndiffStream,
}

impl TextDeltaApplierFileSync {
    pub(crate) fn new(base: Option<std::fs::File>) -> Result<Self, SvnError> {
        let base_len = match base.as_ref() {
            Some(file) => file.metadata()?.len(),
            None => 0,
        };
        Ok(Self {
            base,
            base_len,
            stream: SvndiffStream::default(),
        })
    }

    pub(crate) fn push<W: Write>(&mut self, chunk: &[u8], out: &mut W) -> Result<(), SvnError> {
        self.stream.push(chunk)?;
        while let Some((version, window, ins_wire, new_wire)) = self.stream.next_window()? {
            let instructions = decode_section(version, &ins_wire, MAX_INSTRUCTION_SECTION_LEN)?;
            let new_data = decode_section(version, &new_wire, DELTA_WINDOW_MAX)?;
            let source_view = self.read_source_view(&window)?;
            let data =
                apply_window_source(&source_view, window.tview_len, &instructions, &new_data)?;
            out.write_all(&data)?;
        }
        Ok(())
    }

    pub(crate) fn finish<W: Write>(mut self, out: &mut W) -> Result<(), SvnError> {
        if self.stream.is_identity() {
            if let Some(mut base) = self.base.take() {
                base.seek(SeekFrom::Start(0))?;
                let _ = std::io::copy(&mut base, out)?;
            }
            return Ok(());
        }
        self.stream.finish()
    }

    fn read_source_view(&mut self, window: &WindowHeader) -> Result<Vec<u8>, SvnError> {
        if window.sview_len == 0 {
            return Ok(Vec::new());
        }

        let end = window
            .sview_offset
            .checked_add(window.sview_len as u64)
            .ok_or_else(|| SvnError::Protocol("svndiff source view overflow".into()))?;
        if end > self.base_len {
            return Err(SvnError::Protocol(
                "svndiff source view out of bounds for base".into(),
            ));
        }

        let Some(base) = self.base.as_mut() else {
            return Err(SvnError::Protocol(
                "svndiff source view out of bounds for base".into(),
            ));
        };
        base.seek(SeekFrom::Start(window.sview_offset))?;
        let mut buf = vec![0u8; window.sview_len];
        base.read_exact(&mut buf)?;
        Ok(buf)
    }
}

#[derive(Debug)]
pub(crate) struct TextDeltaApplierFile {
    base: Option<tokio::fs::File>,
    base_len: u64,
    stream: SvndiffStream,
}

impl TextDeltaApplierFile {
    pub(crate) async fn new(base: Option<tokio::fs::File>) -> Result<Self, SvnError> {
        let base_len = match base.as_ref() {
            Some(file) => file.metadata().await?.len(),
            None => 0,
        };
        Ok(Self {
            base,
            base_len,
            stream: SvndiffStream::default(),
        })
    }

    pub(crate) async fn push<W: AsyncWrite + Unpin>(
        &mut self,
        chunk: &[u8],
        out: &mut W,
    ) -> Result<(), SvnError> {
        self.stream.push(chunk)?;
        while let Some((version, window, ins_wire, new_wire)) = self.stream.next_window()? {
            let instructions = decode_section(version, &ins_wire, MAX_INSTRUCTION_SECTION_LEN)?;
            let new_data = decode_section(version, &new_wire, DELTA_WINDOW_MAX)?;
            let source_view = self.read_source_view(&window).await?;
            let data =
                apply_window_source(&source_view, window.tview_len, &instructions, &new_data)?;
            out.write_all(&data).await?;
        }
        Ok(())
    }

    pub(crate) async fn finish<W: AsyncWrite + Unpin>(
        mut self,
        out: &mut W,
    ) -> Result<(), SvnError> {
        if self.stream.is_identity() {
            if let Some(mut base) = self.base.take() {
                base.seek(SeekFrom::Start(0)).await?;
                let _ = tokio::io::copy(&mut base, out).await?;
            }
            return Ok(());
        }
        self.stream.finish()
    }

    async fn read_source_view(&mut self, window: &WindowHeader) -> Result<Vec<u8>, SvnError> {
        if window.sview_len == 0 {
            return Ok(Vec::new());
        }

        let end = window
            .sview_offset
            .checked_add(window.sview_len as u64)
            .ok_or_else(|| SvnError::Protocol("svndiff source view overflow".into()))?;
        if end > self.base_len {
            return Err(SvnError::Protocol(
                "svndiff source view out of bounds for base".into(),
            ));
        }

        let Some(base) = self.base.as_mut() else {
            return Err(SvnError::Protocol(
                "svndiff source view out of bounds for base".into(),
            ));
        };
        base.seek(SeekFrom::Start(window.sview_offset)).await?;
        let mut buf = vec![0u8; window.sview_len];
        base.read_exact(&mut buf).await?;
        Ok(buf)
    }
}

/// A fully recorded textdelta stream for one file token.
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordedTextDelta {
    /// Repository-relative path, if known (from `open-file` / `add-file`).
    pub path: Option<String>,
    /// File token associated with this delta stream.
    pub file_token: String,
    /// Base checksum announced by the server (if any).
    pub base_checksum: Option<String>,
    /// Raw svndiff chunks as received from the server.
    pub chunks: Vec<Vec<u8>>,
    /// Optional text checksum announced on `close-file`.
    pub text_checksum: Option<String>,
}

#[derive(Clone, Debug, Default)]
struct PendingTextDelta {
    path: Option<String>,
    base_checksum: Option<String>,
    chunks: Vec<Vec<u8>>,
}

/// Records `apply-textdelta` streams from an editor drive.
///
/// This is a helper for `update`/`diff`/`replay`-style operations where the
/// server emits `apply-textdelta` and `textdelta-chunk` events. The recorder
/// stores raw svndiff chunks, which can later be applied with
/// [`apply_textdelta`].
///
/// This collector is in-memory and may use significant RAM for large edits.
#[derive(Debug, Default)]
pub struct TextDeltaRecorder {
    file_paths: HashMap<String, String>,
    pending: HashMap<String, PendingTextDelta>,
    last_completed: HashMap<String, usize>,
    completed: Vec<RecordedTextDelta>,
}

impl TextDeltaRecorder {
    /// Creates an empty recorder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns all completed textdeltas recorded so far.
    pub fn completed(&self) -> &[RecordedTextDelta] {
        &self.completed
    }

    /// Takes all completed textdeltas, leaving the recorder empty.
    pub fn take_completed(&mut self) -> Vec<RecordedTextDelta> {
        self.last_completed.clear();
        std::mem::take(&mut self.completed)
    }
}

impl EditorEventHandler for TextDeltaRecorder {
    fn on_event(&mut self, event: EditorEvent) -> Result<(), SvnError> {
        match event {
            EditorEvent::AddFile {
                path, file_token, ..
            }
            | EditorEvent::OpenFile {
                path, file_token, ..
            } => {
                self.file_paths.insert(file_token, path);
            }
            EditorEvent::ApplyTextDelta {
                file_token,
                base_checksum,
            } => {
                if self.pending.contains_key(&file_token) {
                    return Err(SvnError::Protocol(format!(
                        "duplicate apply-textdelta for file token '{file_token}'"
                    )));
                }

                let path = self.file_paths.get(&file_token).cloned();
                self.pending.insert(
                    file_token,
                    PendingTextDelta {
                        path,
                        base_checksum,
                        chunks: Vec::new(),
                    },
                );
            }
            EditorEvent::TextDeltaChunk { file_token, chunk } => {
                let pending = self.pending.get_mut(&file_token).ok_or_else(|| {
                    SvnError::Protocol(format!(
                        "textdelta-chunk for unknown file token '{file_token}'"
                    ))
                })?;
                pending.chunks.push(chunk);
            }
            EditorEvent::TextDeltaEnd { file_token } => {
                let pending = self.pending.remove(&file_token).ok_or_else(|| {
                    SvnError::Protocol(format!(
                        "textdelta-end for unknown file token '{file_token}'"
                    ))
                })?;

                let record = RecordedTextDelta {
                    path: pending.path,
                    file_token: file_token.clone(),
                    base_checksum: pending.base_checksum,
                    chunks: pending.chunks,
                    text_checksum: None,
                };
                self.completed.push(record);
                self.last_completed
                    .insert(file_token, self.completed.len() - 1);
            }
            EditorEvent::CloseFile {
                file_token,
                text_checksum,
            } => {
                if let Some(text_checksum) = text_checksum
                    && let Some(&idx) = self.last_completed.get(&file_token)
                    && let Some(record) = self.completed.get_mut(idx)
                {
                    record.text_checksum = Some(text_checksum);
                }
                self.file_paths.remove(&file_token);
            }
            EditorEvent::CloseEdit | EditorEvent::AbortEdit | EditorEvent::FinishReplay => {
                if !self.pending.is_empty() {
                    return Err(SvnError::Protocol(
                        "editor drive ended with an unfinished textdelta".into(),
                    ));
                }
            }
            _ => {}
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use tokio::io::AsyncWrite;

    use super::*;
    use crate::svndiff::{SvndiffVersion as EncVersion, encode_fulltext_with_options};

    fn run_async<T>(f: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(f)
    }

    #[derive(Default)]
    struct VecWriter {
        buf: Vec<u8>,
    }

    impl AsyncWrite for VecWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.buf.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn svndiff0_window(
        sview_offset: u8,
        sview_len: u8,
        tview_len: u8,
        instructions: &[u8],
        new_data: &[u8],
    ) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"SVN\0");
        out.push(sview_offset);
        out.push(sview_len);
        out.push(tview_len);
        out.push(instructions.len() as u8);
        out.push(new_data.len() as u8);
        out.extend_from_slice(instructions);
        out.extend_from_slice(new_data);
        out
    }

    #[test]
    fn apply_svndiff0_source_and_new() {
        run_async(async {
            let base = b"abcdef";
            let delta = svndiff0_window(
                0,
                6,
                6,
                &[
                    0x02, 0x00, // src 2 @0
                    0x82, // new 2
                    0x02, 0x04, // src 2 @4
                ],
                b"XY",
            );

            let mut out = VecWriter::default();
            let mut applier = TextDeltaApplier::new(base);
            applier.push(&delta[..3], &mut out).await.unwrap();
            applier.push(&delta[3..], &mut out).await.unwrap();
            applier.finish(&mut out).await.unwrap();
            assert_eq!(out.buf, b"abXYef");
        });
    }

    #[test]
    fn apply_svndiff0_target_copy_with_overlap() {
        run_async(async {
            let delta = svndiff0_window(
                0,
                0,
                6,
                &[
                    0x81, // new 1
                    0x45, 0x00, // tgt 5 @0
                ],
                b"a",
            );

            let mut out = VecWriter::default();
            apply_textdelta(&[], [&delta[..]], &mut out).await.unwrap();
            assert_eq!(out.buf, b"aaaaaa");
        });
    }

    #[test]
    fn apply_empty_delta_is_identity() {
        run_async(async {
            let mut out = VecWriter::default();
            apply_textdelta(b"base", std::iter::empty::<&[u8]>(), &mut out)
                .await
                .unwrap();
            assert_eq!(out.buf, b"base");
        });
    }

    #[test]
    fn apply_svndiff1_fulltext_roundtrips() {
        run_async(async {
            let contents = vec![0u8; 4096];
            let delta =
                encode_fulltext_with_options(EncVersion::V1, &contents, 5, 64 * 1024).unwrap();

            let mut out = VecWriter::default();
            let split = (delta.len() / 2).max(1).min(delta.len());
            apply_textdelta(&[], [&delta[..split], &delta[split..]], &mut out)
                .await
                .unwrap();
            assert_eq!(out.buf, contents);
        });
    }

    #[test]
    fn apply_svndiff2_fulltext_roundtrips() {
        run_async(async {
            let contents = vec![0u8; 4096];
            let delta =
                encode_fulltext_with_options(EncVersion::V2, &contents, 5, 64 * 1024).unwrap();

            let mut out = VecWriter::default();
            let split = (delta.len() / 3).max(1).min(delta.len());
            let split2 = (split * 2).min(delta.len());
            apply_textdelta(
                &[],
                [&delta[..split], &delta[split..split2], &delta[split2..]],
                &mut out,
            )
            .await
            .unwrap();
            assert_eq!(out.buf, contents);
        });
    }

    #[test]
    fn recorder_tracks_chunks_and_checksums() {
        let mut recorder = TextDeltaRecorder::new();

        crate::editor::EditorEventHandler::on_event(
            &mut recorder,
            EditorEvent::OpenFile {
                path: "trunk/hello.txt".to_string(),
                dir_token: "d1".to_string(),
                file_token: "f1".to_string(),
                rev: 1,
            },
        )
        .unwrap();
        crate::editor::EditorEventHandler::on_event(
            &mut recorder,
            EditorEvent::ApplyTextDelta {
                file_token: "f1".to_string(),
                base_checksum: Some("base".to_string()),
            },
        )
        .unwrap();
        crate::editor::EditorEventHandler::on_event(
            &mut recorder,
            EditorEvent::TextDeltaChunk {
                file_token: "f1".to_string(),
                chunk: vec![1, 2, 3],
            },
        )
        .unwrap();
        crate::editor::EditorEventHandler::on_event(
            &mut recorder,
            EditorEvent::TextDeltaEnd {
                file_token: "f1".to_string(),
            },
        )
        .unwrap();
        crate::editor::EditorEventHandler::on_event(
            &mut recorder,
            EditorEvent::CloseFile {
                file_token: "f1".to_string(),
                text_checksum: Some("text".to_string()),
            },
        )
        .unwrap();

        assert_eq!(recorder.completed().len(), 1);
        let d = &recorder.completed()[0];
        assert_eq!(d.path.as_deref(), Some("trunk/hello.txt"));
        assert_eq!(d.file_token, "f1");
        assert_eq!(d.base_checksum.as_deref(), Some("base"));
        assert_eq!(d.text_checksum.as_deref(), Some("text"));
        assert_eq!(d.chunks, vec![vec![1, 2, 3]]);
    }
}
