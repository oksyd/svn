use super::*;

pub(super) struct LimitedVecWriter {
    buf: Vec<u8>,
    max_bytes: u64,
}

impl LimitedVecWriter {
    pub(super) fn new(max_bytes: u64) -> Self {
        Self {
            buf: Vec::new(),
            max_bytes,
        }
    }

    pub(super) fn into_inner(self) -> Vec<u8> {
        self.buf
    }
}

impl AsyncWrite for LimitedVecWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let next = (self.buf.len() as u64).saturating_add(buf.len() as u64);
        if next > self.max_bytes {
            return Poll::Ready(Err(std::io::Error::other(format!(
                "buffer exceeds limit {}",
                self.max_bytes
            ))));
        }

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
