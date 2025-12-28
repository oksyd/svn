/// Low-level helpers for encoding `ra_svn` wire tokens without heap allocations.
///
/// This module is internal and is intentionally not part of the public API.
use super::SvnItem;

pub(crate) struct WireEncoder<'a> {
    out: &'a mut Vec<u8>,
}

impl<'a> WireEncoder<'a> {
    pub(crate) fn new(out: &'a mut Vec<u8>) -> Self {
        Self { out }
    }

    pub(crate) fn word(&mut self, word: &str) {
        self.out.extend_from_slice(word.as_bytes());
        self.out.push(b' ');
    }

    pub(crate) fn number(&mut self, n: u64) {
        encode_decimal_u64(n, self.out);
        self.out.push(b' ');
    }

    pub(crate) fn bool(&mut self, b: bool) {
        if b {
            self.out.extend_from_slice(b"true ");
        } else {
            self.out.extend_from_slice(b"false ");
        }
    }

    pub(crate) fn string_bytes(&mut self, bytes: &[u8]) {
        encode_decimal_usize(bytes.len(), self.out);
        self.out.push(b':');
        self.out.extend_from_slice(bytes);
        self.out.push(b' ');
    }

    pub(crate) fn string_str(&mut self, s: &str) {
        self.string_bytes(s.as_bytes());
    }

    pub(crate) fn list_start(&mut self) {
        self.out.extend_from_slice(b"( ");
    }

    pub(crate) fn list_end(&mut self) {
        self.out.extend_from_slice(b") ");
    }

    pub(crate) fn newline(&mut self) {
        self.out.push(b'\n');
    }
}

pub(crate) fn encode_decimal_u64(mut n: u64, out: &mut Vec<u8>) {
    if n == 0 {
        out.push(b'0');
        return;
    }
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    while n > 0 {
        let digit = (n % 10) as u8;
        n /= 10;
        i -= 1;
        buf[i] = b'0' + digit;
    }
    out.extend_from_slice(&buf[i..]);
}

pub(crate) fn encode_decimal_usize(n: usize, out: &mut Vec<u8>) {
    encode_decimal_u64(n as u64, out);
}

pub(crate) fn encode_command_item(command: &str, params: &SvnItem, out: &mut Vec<u8>) {
    out.extend_from_slice(b"( ");
    out.extend_from_slice(command.as_bytes());
    out.push(b' ');
    super::encode_item(params, out);
    out.extend_from_slice(b") ");
}
