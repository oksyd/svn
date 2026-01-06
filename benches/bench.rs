//! Benchmarks for the `svn` crate.
//!
//! Run with:
//! - `cargo bench`

#![allow(missing_docs)]

use std::hint::black_box;
use std::io::Write;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use flate2::Compression;
use flate2::write::ZlibEncoder;
use svn::{SvnUrl, apply_textdelta_sync};

const ZLIB_MIN_COMPRESS_SIZE: usize = 512;

#[derive(Clone, Copy, Debug)]
enum SvndiffVersion {
    V0 = 0,
    V1 = 1,
    V2 = 2,
}

impl SvndiffVersion {
    fn header(self) -> [u8; 4] {
        match self {
            Self::V0 => *b"SVN\0",
            Self::V1 => *b"SVN\x01",
            Self::V2 => *b"SVN\x02",
        }
    }
}

fn abort_with_error(message: &str) -> ! {
    eprintln!("{message}");
    std::process::abort();
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

fn encode_new_instruction(len: usize, out: &mut Vec<u8>) {
    let len = len as u64;
    if (len >> 6) == 0 {
        out.push((0x2 << 6) | (len as u8));
    } else {
        out.push((0x2 << 6) as u8);
        encode_uint(len, out);
    }
}

fn encode_zlib_section(data: &[u8], zlib_level: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_uint(data.len() as u64, &mut out);

    if data.len() < ZLIB_MIN_COMPRESS_SIZE || zlib_level == 0 {
        out.extend_from_slice(data);
        return out;
    }

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::new(zlib_level));
    if encoder.write_all(data).is_err() {
        abort_with_error("zlib encoder write failed");
    }
    let compressed = match encoder.finish() {
        Ok(v) => v,
        Err(_) => abort_with_error("zlib encoder finish failed"),
    };

    if compressed.len() >= data.len() {
        out.extend_from_slice(data);
    } else {
        out.extend_from_slice(&compressed);
    }
    out
}

fn encode_lz4_section(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    encode_uint(data.len() as u64, &mut out);

    let compressed = lz4_flex::compress(data);
    if compressed.len() >= data.len() {
        out.extend_from_slice(data);
    } else {
        out.extend_from_slice(&compressed);
    }
    out
}

fn encode_fulltext(version: SvndiffVersion, contents: &[u8], zlib_level: u32) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&version.header());

    let mut instructions = Vec::new();
    if !contents.is_empty() {
        encode_new_instruction(contents.len(), &mut instructions);
    }

    let (instructions_wire, newdata_wire) = match version {
        SvndiffVersion::V0 => (instructions, contents.to_vec()),
        SvndiffVersion::V1 => (
            encode_zlib_section(&instructions, zlib_level),
            encode_zlib_section(contents, zlib_level),
        ),
        SvndiffVersion::V2 => (
            encode_lz4_section(&instructions),
            encode_lz4_section(contents),
        ),
    };

    encode_uint(0, &mut out); // sview_offset
    encode_uint(0, &mut out); // sview_len
    encode_uint(contents.len() as u64, &mut out); // tview_len
    encode_uint(instructions_wire.len() as u64, &mut out); // instructions len (wire)
    encode_uint(newdata_wire.len() as u64, &mut out); // newdata len (wire)
    out.extend_from_slice(&instructions_wire);
    out.extend_from_slice(&newdata_wire);
    out
}

fn bench_textdelta_apply_fulltext(c: &mut Criterion) {
    let mut group = c.benchmark_group("textdelta_apply_fulltext");

    for &size in &[4 * 1024usize, 256 * 1024] {
        let contents = vec![0u8; size];
        group.throughput(Throughput::Bytes(size as u64));

        for version in [SvndiffVersion::V0, SvndiffVersion::V1, SvndiffVersion::V2] {
            let delta = encode_fulltext(version, &contents, 6);
            let chunks_1k: Vec<&[u8]> = delta.chunks(1024).collect();

            group.bench_with_input(
                BenchmarkId::new(format!("{version:?}/1k_chunks"), size),
                &chunks_1k,
                |b, chunks| {
                    b.iter(|| {
                        let mut out = Vec::with_capacity(size);
                        if apply_textdelta_sync(&[], chunks.iter(), &mut out).is_err() {
                            abort_with_error("apply_textdelta_sync failed");
                        }
                        black_box(out.len());
                    });
                },
            );

            group.bench_with_input(
                BenchmarkId::new(format!("{version:?}/single_chunk"), size),
                &delta,
                |b, delta| {
                    b.iter(|| {
                        let mut out = Vec::with_capacity(size);
                        if apply_textdelta_sync(&[], [delta.as_slice()], &mut out).is_err() {
                            abort_with_error("apply_textdelta_sync failed");
                        }
                        black_box(out.len());
                    });
                },
            );
        }
    }

    group.finish();
}

fn bench_url_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("url_parse");
    for input in [
        "svn://example.com/repo",
        "svn+ssh://alice@example.com/repo",
        "svn://[2001:db8::1]/repo",
    ] {
        group.bench_with_input(BenchmarkId::from_parameter(input), input, |b, input| {
            b.iter(|| {
                let url = match SvnUrl::parse(black_box(input)) {
                    Ok(url) => url,
                    Err(_) => abort_with_error("SvnUrl::parse failed for benchmark input"),
                };
                black_box(url.url);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_textdelta_apply_fulltext, bench_url_parse);
criterion_main!(benches);
