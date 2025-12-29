use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use crate::SvnError;

const ZLIB_MIN_COMPRESS_SIZE: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SvndiffVersion {
    V0 = 0,
    V1 = 1,
    V2 = 2,
}

impl SvndiffVersion {
    pub(crate) fn header(self) -> [u8; 4] {
        match self {
            Self::V0 => *b"SVN\0",
            Self::V1 => *b"SVN\x01",
            Self::V2 => *b"SVN\x02",
        }
    }
}

pub(crate) fn encode_fulltext_with_options(
    version: SvndiffVersion,
    contents: &[u8],
    zlib_level: u32,
    window_size: usize,
) -> Result<Vec<u8>, SvnError> {
    let window_size = window_size.max(1);

    let mut out = Vec::new();
    out.extend_from_slice(&version.header());

    for chunk in contents.chunks(window_size) {
        encode_insertion_window(version, chunk, zlib_level, &mut out)?;
    }

    if contents.is_empty() {
        // A zero-length file still needs at least one window.
        encode_insertion_window(version, &[], zlib_level, &mut out)?;
    }

    Ok(out)
}

pub(crate) fn encode_insertion_window(
    version: SvndiffVersion,
    new_data: &[u8],
    zlib_level: u32,
    out: &mut Vec<u8>,
) -> Result<(), SvnError> {
    let mut instructions = Vec::new();
    encode_new_instruction(new_data.len(), &mut instructions);

    let (instructions_wire, newdata_wire) = match version {
        SvndiffVersion::V0 => (instructions, new_data.to_vec()),
        SvndiffVersion::V1 => (
            compress_zlib(&instructions, zlib_level)?,
            compress_zlib(new_data, zlib_level)?,
        ),
        SvndiffVersion::V2 => (compress_lz4(&instructions)?, compress_lz4(new_data)?),
    };

    encode_uint(0, out); // sview_offset
    encode_uint(0, out); // sview_len
    encode_uint(new_data.len() as u64, out); // tview_len
    encode_uint(instructions_wire.len() as u64, out); // instructions len (wire)
    encode_uint(newdata_wire.len() as u64, out); // newdata len (wire)

    out.extend_from_slice(&instructions_wire);
    out.extend_from_slice(&newdata_wire);
    Ok(())
}

fn encode_new_instruction(len: usize, out: &mut Vec<u8>) {
    let len = len as u64;
    if (len >> 6) == 0 {
        out.push((0x2 << 6) | (len as u8));
    } else {
        out.push((0x2 << 6) as u8);
        encode_uint(len, out);
    }
}

fn encode_uint(val: u64, out: &mut Vec<u8>) {
    let mut v = val >> 7;
    let mut n = 1u32;
    while v > 0 {
        v >>= 7;
        n += 1;
    }

    while n > 1 {
        n -= 1;
        out.push((((val >> (n * 7)) | 0x80) & 0xff) as u8);
    }
    out.push((val & 0x7f) as u8);
}

fn compress_zlib(data: &[u8], zlib_level: u32) -> Result<Vec<u8>, SvnError> {
    let mut out = Vec::new();
    encode_uint(data.len() as u64, &mut out);

    if data.len() < ZLIB_MIN_COMPRESS_SIZE || zlib_level == 0 {
        out.extend_from_slice(data);
        return Ok(out);
    }

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(zlib_level));
    encoder
        .write_all(data)
        .map_err(|err| SvnError::Protocol(format!("zlib encode failed: {err}")))?;
    let compressed = encoder
        .finish()
        .map_err(|err| SvnError::Protocol(format!("zlib finish failed: {err}")))?;

    if compressed.len() >= data.len() {
        out.extend_from_slice(data);
    } else {
        out.extend_from_slice(&compressed);
    }
    Ok(out)
}

fn compress_lz4(data: &[u8]) -> Result<Vec<u8>, SvnError> {
    let mut out = Vec::new();
    encode_uint(data.len() as u64, &mut out);

    let compressed = lz4_flex::compress(data);
    if compressed.len() >= data.len() {
        out.extend_from_slice(data);
    } else {
        out.extend_from_slice(&compressed);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::io::Read;

    use super::*;

    fn decode_uint(mut input: &[u8]) -> Option<(u64, &[u8])> {
        let mut val = 0u64;
        loop {
            let (&b, rest) = input.split_first()?;
            input = rest;
            val = val.checked_shl(7)?.checked_add(u64::from(b & 0x7f))?;
            if (b & 0x80) == 0 {
                return Some((val, input));
            }
        }
    }

    fn decode_zlib_section(input: &[u8]) -> Vec<u8> {
        let (orig_len, rest) = decode_uint(input).unwrap();
        let orig_len = orig_len as usize;
        if rest.len() == orig_len {
            return rest.to_vec();
        }
        let mut decoder = flate2::read::ZlibDecoder::new(rest);
        let mut out = Vec::new();
        decoder.read_to_end(&mut out).unwrap();
        out
    }

    fn decode_lz4_section(input: &[u8]) -> Vec<u8> {
        let (orig_len, rest) = decode_uint(input).unwrap();
        let orig_len = orig_len as usize;
        if rest.len() == orig_len {
            return rest.to_vec();
        }
        lz4_flex::decompress(rest, orig_len).unwrap()
    }

    fn split_single_window(encoded: &[u8]) -> (u64, u64, u64, Vec<u8>, Vec<u8>) {
        let mut input = encoded;
        let (sview_offset, rest) = decode_uint(input).unwrap();
        input = rest;
        let (sview_len, rest) = decode_uint(input).unwrap();
        input = rest;
        let (tview_len, rest) = decode_uint(input).unwrap();
        input = rest;
        let (ins_len, rest) = decode_uint(input).unwrap();
        input = rest;
        let (new_len, rest) = decode_uint(input).unwrap();
        input = rest;

        let ins_len = ins_len as usize;
        let new_len = new_len as usize;
        let instructions = input[..ins_len].to_vec();
        let newdata = input[ins_len..][..new_len].to_vec();
        (sview_offset, sview_len, tview_len, instructions, newdata)
    }

    #[test]
    fn svndiff_v0_fulltext_small_matches_known_bytes() {
        let bytes = encode_fulltext_with_options(SvndiffVersion::V0, b"abc", 0, 64).unwrap();
        assert_eq!(
            bytes,
            [
                b'S',
                b'V',
                b'N',
                0,        // header
                0,        // sview_offset
                0,        // sview_len
                3,        // tview_len
                1,        // instructions_len
                3,        // newdata_len
                0x80 | 3, // insert 3 bytes
                b'a',
                b'b',
                b'c',
            ]
        );
    }

    #[test]
    fn svndiff_v1_small_roundtrips_sections() {
        let bytes = encode_fulltext_with_options(SvndiffVersion::V1, b"abc", 5, 64).unwrap();
        assert_eq!(&bytes[..4], b"SVN\x01");

        let (sview_offset, sview_len, tview_len, instructions_wire, newdata_wire) =
            split_single_window(&bytes[4..]);
        assert_eq!((sview_offset, sview_len, tview_len), (0, 0, 3));

        let instructions = decode_zlib_section(&instructions_wire);
        let newdata = decode_zlib_section(&newdata_wire);
        assert_eq!(instructions, vec![0x80 | 3]);
        assert_eq!(newdata, b"abc");
    }

    #[test]
    fn svndiff_v2_small_roundtrips_sections() {
        let bytes = encode_fulltext_with_options(SvndiffVersion::V2, b"abc", 5, 64).unwrap();
        assert_eq!(&bytes[..4], b"SVN\x02");

        let (sview_offset, sview_len, tview_len, instructions_wire, newdata_wire) =
            split_single_window(&bytes[4..]);
        assert_eq!((sview_offset, sview_len, tview_len), (0, 0, 3));

        let instructions = decode_lz4_section(&instructions_wire);
        let newdata = decode_lz4_section(&newdata_wire);
        assert_eq!(instructions, vec![0x80 | 3]);
        assert_eq!(newdata, b"abc");
    }

    #[test]
    fn svndiff_v1_large_roundtrips_and_compresses_newdata() {
        let contents = vec![0u8; 4096];
        let bytes =
            encode_fulltext_with_options(SvndiffVersion::V1, &contents, 5, 16 * 1024).unwrap();
        assert_eq!(&bytes[..4], b"SVN\x01");

        let (_sview_offset, _sview_len, tview_len, _instructions_wire, newdata_wire) =
            split_single_window(&bytes[4..]);
        assert_eq!(tview_len as usize, contents.len());

        let (orig_len, rest) = decode_uint(&newdata_wire).unwrap();
        assert_eq!(orig_len as usize, contents.len());
        assert!(rest.len() < contents.len());

        let decoded = decode_zlib_section(&newdata_wire);
        assert_eq!(decoded, contents);
    }

    #[test]
    fn svndiff_v2_large_roundtrips_and_compresses_newdata() {
        let contents = vec![0u8; 4096];
        let bytes =
            encode_fulltext_with_options(SvndiffVersion::V2, &contents, 5, 16 * 1024).unwrap();
        assert_eq!(&bytes[..4], b"SVN\x02");

        let (_sview_offset, _sview_len, tview_len, _instructions_wire, newdata_wire) =
            split_single_window(&bytes[4..]);
        assert_eq!(tview_len as usize, contents.len());

        let (orig_len, rest) = decode_uint(&newdata_wire).unwrap();
        assert_eq!(orig_len as usize, contents.len());
        assert!(rest.len() < contents.len());

        let decoded = decode_lz4_section(&newdata_wire);
        assert_eq!(decoded, contents);
    }

    #[test]
    fn svndiff_v0_fulltext_empty_still_emits_a_window() {
        let bytes = encode_fulltext_with_options(SvndiffVersion::V0, b"", 0, 64).unwrap();
        assert_eq!(
            bytes,
            [
                b'S', b'V', b'N', 0, // header
                0, 0, 0,    // sview_offset/sview_len/tview_len
                1,    // instructions_len
                0,    // newdata_len
                0x80, // insert 0 bytes (length in low bits)
            ]
        );
    }
}
