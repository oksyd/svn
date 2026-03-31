use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SvndiffVersion {
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
pub(super) struct WindowHeader {
    pub(super) sview_offset: u64,
    pub(super) sview_len: usize,
    pub(super) tview_len: usize,
    pub(super) ins_len: usize,
    pub(super) new_len: usize,
    pub(super) header_len: usize,
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
pub(super) struct SvndiffStream {
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
    pub(super) fn push(&mut self, chunk: &[u8]) -> Result<(), SvnError> {
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

    pub(super) fn is_identity(&self) -> bool {
        !self.any_input
    }

    pub(super) fn next_window(&mut self) -> Result<Option<ParsedWindow>, SvnError> {
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

    pub(super) fn finish(&self) -> Result<(), SvnError> {
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

pub(super) fn decode_section(
    version: SvndiffVersion,
    wire: &[u8],
    limit: usize,
) -> Result<Vec<u8>, SvnError> {
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

pub(super) fn apply_window(
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

pub(super) fn apply_window_source(
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
