#![allow(clippy::unwrap_used)]

use std::pin::Pin;
use std::task::{Context, Poll};

use proptest::prelude::*;
use tokio::io::AsyncWrite;

use super::*;
use crate::svndiff::{SvndiffVersion as EncVersion, encode_fulltext_with_options};
use crate::test_support::run_async;

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

fn split_slices_by_seeds<'a>(bytes: &'a [u8], seeds: &[u8]) -> Vec<&'a [u8]> {
    if bytes.is_empty() {
        return vec![bytes];
    }

    let mut out = Vec::new();
    let mut start = 0usize;
    for &seed in seeds {
        if start == bytes.len() {
            break;
        }
        let remaining = bytes.len() - start;
        let take = (seed as usize) % (remaining + 1);
        out.push(&bytes[start..start + take]);
        start += take;
    }
    if start < bytes.len() {
        out.push(&bytes[start..]);
    }
    out
}

fn arb_svndiff_version() -> impl Strategy<Value = EncVersion> {
    prop_oneof![
        Just(EncVersion::V0),
        Just(EncVersion::V1),
        Just(EncVersion::V2),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 32,
        .. ProptestConfig::default()
    })]

    #[test]
    fn fulltext_svndiff_applies_via_sync_textdelta(
        version in arb_svndiff_version(),
        zlib_level in 0u32..=9u32,
        window_size in 1usize..=8192,
        base in prop::collection::vec(any::<u8>(), 0..=1024),
        contents in prop::collection::vec(any::<u8>(), 0..=16 * 1024),
        seeds in prop::collection::vec(any::<u8>(), 0..=64),
    ) {
        let delta = encode_fulltext_with_options(version, &contents, zlib_level, window_size).unwrap();
        let chunks = split_slices_by_seeds(&delta, &seeds);

        let mut out = Vec::new();
        apply_textdelta_sync(&base, chunks.iter().copied(), &mut out).unwrap();
        assert_eq!(out, contents);
    }

    #[test]
    fn truncated_svndiff_is_rejected(
        version in arb_svndiff_version(),
        zlib_level in 0u32..=9u32,
        window_size in 1usize..=8192,
        contents in prop::collection::vec(any::<u8>(), 0..=16 * 1024),
    ) {
        let delta = encode_fulltext_with_options(version, &contents, zlib_level, window_size).unwrap();
        prop_assume!(delta.len() > 1);

        let mut out = Vec::new();
        let err = apply_textdelta_sync(&[], [&delta[..delta.len() - 1]], &mut out).unwrap_err();
        prop_assert!(matches!(err, SvnError::Protocol(_)));
    }

    #[test]
    fn apply_textdelta_sync_never_panics_on_random_input(
        base in prop::collection::vec(any::<u8>(), 0..=256),
        delta in prop::collection::vec(any::<u8>(), 0..=2048),
    ) {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut out = Vec::new();
            let _ = apply_textdelta_sync(&base, [&delta], &mut out);
        }));
        prop_assert!(res.is_ok());
    }
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
        let delta = encode_fulltext_with_options(EncVersion::V1, &contents, 5, 64 * 1024).unwrap();

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
        let delta = encode_fulltext_with_options(EncVersion::V2, &contents, 5, 64 * 1024).unwrap();

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
