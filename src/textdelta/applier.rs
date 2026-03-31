use super::decode::{
    SvndiffStream, WindowHeader, apply_window, apply_window_source, decode_section,
};
use super::*;

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
