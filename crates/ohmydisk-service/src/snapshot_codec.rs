//! Versioned snapshot encoding. JSON streams through bounded compression buffers.

use lz4_flex::frame::{FrameDecoder, FrameEncoder, FrameInfo};
use ohmydisk_core::{AppError, Snapshot};
use std::io::{self, Read, Write};

pub(crate) const CODEC: &str = "lz4-json-v1";

pub(crate) struct EncodedSnapshot {
    pub payload: Vec<u8>,
    pub json_bytes: u64,
}

fn invalid(message: &str) -> AppError {
    AppError::new("state_error", message)
}

struct CountingWriter<W> {
    inner: W,
    bytes: u64,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.bytes = self
            .bytes
            .checked_add(written as u64)
            .ok_or_else(|| io::Error::other("Snapshot JSON length overflow"))?;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// A complete frame ends before its underlying reader reaches EOF. In contrast,
/// `FrameDecoder` treats an `UnexpectedEof` while reading a block header as EOF, so
/// ordinary slice input alone would accept some truncated frames.
struct FrameInput<'a> {
    remaining: &'a [u8],
}

impl Read for FrameInput<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !buffer.is_empty() && self.remaining.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Truncated snapshot frame",
            ));
        }
        self.remaining.read(buffer)
    }
}

pub(crate) fn encode(snapshot: &Snapshot) -> Result<EncodedSnapshot, AppError> {
    let encoder: FrameEncoder<Vec<u8>> =
        FrameEncoder::with_frame_info(FrameInfo::new().content_checksum(true), Vec::new());
    let mut writer = CountingWriter {
        inner: encoder,
        bytes: 0,
    };
    serde_json::to_writer(&mut writer, snapshot)
        .map_err(|_| invalid("Cannot encode snapshot JSON"))?;
    let payload = writer
        .inner
        .finish()
        .map_err(|_| invalid("Cannot finish snapshot frame"))?;
    Ok(EncodedSnapshot {
        payload,
        json_bytes: writer.bytes,
    })
}

pub(crate) fn decode(codec: &str, payload: &[u8], json_bytes: u64) -> Result<Snapshot, AppError> {
    if codec != CODEC {
        return Err(invalid("Unsupported snapshot codec"));
    }
    let limit = json_bytes
        .checked_add(1)
        .ok_or_else(|| invalid("Invalid snapshot JSON length"))?;
    let decoder = FrameDecoder::new(FrameInput { remaining: payload });
    // Read one byte beyond the recorded length to detect a longer expansion.
    // No fixed maximum excludes large, valid whole-volume snapshots.
    let mut reader = decoder.take(limit);
    // A bounded byte buffer lets serde parse contiguous strings efficiently. This
    // temporarily coexists with the decoded snapshot; it is dropped on return.
    // Never reserve an unchecked recorded length before reading actual data.
    let initial_capacity = usize::try_from(json_bytes)
        .unwrap_or(64 * 1024)
        .min(64 * 1024);
    let mut json = Vec::with_capacity(initial_capacity);
    reader
        .read_to_end(&mut json)
        .map_err(|_| invalid("Cannot decode snapshot frame"))?;
    if reader.limit() != 1 {
        return Err(invalid("Snapshot JSON length does not match"));
    }
    if !reader.into_inner().into_inner().remaining.is_empty() {
        return Err(invalid("Trailing snapshot frame data"));
    }
    // Validate UTF-8 once across the contiguous payload. The string parser can
    // then avoid revalidating every individual node string; no copy is needed.
    let text = std::str::from_utf8(&json).map_err(|_| invalid("Cannot decode snapshot JSON"))?;
    serde_json::from_str(text).map_err(|_| invalid("Cannot decode snapshot JSON"))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ohmydisk_core::{NativePath, Node, NodeKind, ScanIssue};

    pub(crate) fn snapshot() -> Snapshot {
        Snapshot {
            id: "snapshot-fixture".into(),
            scan_id: "scan-fixture".into(),
            root_id: "root".into(),
            root_path: NativePath {
                display: "/fixture/space-é".into(),
                encoded: "L2ZpeHR1cmUvc3BhY2Utw6k=".into(),
            },
            created_at: 123_456,
            complete: false,
            cancelled: true,
            nodes: [
                NodeKind::Directory,
                NodeKind::File,
                NodeKind::Symlink,
                NodeKind::Other,
            ]
            .into_iter()
            .enumerate()
            .map(|(index, kind)| Node {
                id: if index == 0 {
                    "root".into()
                } else {
                    index.to_string()
                },
                parent_id: (index != 0).then(|| "root".into()),
                name: format!("entry-{index}-é\n\""),
                path: NativePath {
                    display: format!("/fixture/entry-{index}"),
                    encoded: format!("encoded-native-path-{index}"),
                },
                kind,
                logical_bytes: u64::MAX - index as u64,
                allocated_bytes: (index % 2 == 0).then_some(index as u64),
                modified_ns: (index % 2 != 0).then_some(u64::MAX),
                identity: format!("identity:{index}"),
                volume_id: "volume:fixture".into(),
                link_count: index as u64 + 1,
                complete: index % 2 == 0,
                is_link_or_reparse: kind == NodeKind::Symlink,
            })
            .collect(),
            issues: vec![ScanIssue {
                node_id: Some("2".into()),
                path: "/fixture/link".into(),
                code: "boundary_skipped".into(),
                message: "Fixture boundary".into(),
            }],
        }
    }

    #[test]
    fn streamed_round_trip_preserves_every_snapshot_and_node_field() {
        let snapshot = snapshot();
        let encoded = encode(&snapshot).unwrap();
        let raw = serde_json::to_vec(&snapshot).unwrap();
        assert_eq!(encoded.json_bytes, raw.len() as u64);
        assert!(encoded.payload.len() < raw.len());
        let decoded = decode(CODEC, &encoded.payload, encoded.json_bytes).unwrap();
        assert_eq!(
            serde_json::to_value(decoded).unwrap(),
            serde_json::to_value(snapshot).unwrap()
        );
    }

    #[test]
    fn every_truncation_and_trailing_frame_is_rejected() {
        let encoded = encode(&snapshot()).unwrap();
        for end in 0..encoded.payload.len() {
            let error = decode(CODEC, &encoded.payload[..end], encoded.json_bytes).unwrap_err();
            assert_eq!(error.code, "state_error", "truncation offset {end}");
        }
        let mut extra_frame = encoded.payload.clone();
        extra_frame.extend_from_slice(&encoded.payload);
        assert_eq!(
            decode(CODEC, &extra_frame, encoded.json_bytes)
                .unwrap_err()
                .code,
            "state_error"
        );
    }

    #[test]
    fn unknown_codec_corruption_bad_json_and_wrong_lengths_are_rejected() {
        let encoded = encode(&snapshot()).unwrap();
        for codec in ["lz4-json-v2", "json", ""] {
            assert_eq!(
                decode(codec, &encoded.payload, encoded.json_bytes)
                    .unwrap_err()
                    .code,
                "state_error"
            );
        }
        let mut corrupt = encoded.payload.clone();
        *corrupt.last_mut().unwrap() ^= 1;
        assert_eq!(
            decode(CODEC, &corrupt, encoded.json_bytes)
                .unwrap_err()
                .code,
            "state_error"
        );
        for length in [
            0,
            1,
            encoded.json_bytes - 1,
            encoded.json_bytes + 1,
            i64::MAX as u64,
            u64::MAX - 1,
            u64::MAX,
        ] {
            assert_eq!(
                decode(CODEC, &encoded.payload, length).unwrap_err().code,
                "state_error"
            );
        }
        let mut json_frame = FrameEncoder::new(Vec::new());
        json_frame.write_all(b"not JSON").unwrap();
        let invalid_json = json_frame.finish().unwrap();
        assert_eq!(
            decode(CODEC, &invalid_json, 8).unwrap_err().code,
            "state_error"
        );
    }

    #[test]
    fn large_snapshot_crosses_compression_blocks_without_a_size_cap() {
        let mut snapshot = snapshot();
        snapshot.nodes[0].name = "large-fixture".repeat(400_000);
        let encoded = encode(&snapshot).unwrap();
        assert!(encoded.json_bytes > 4 * 1024 * 1024);
        let decoded = decode(CODEC, &encoded.payload, encoded.json_bytes).unwrap();
        assert_eq!(decoded.nodes[0].name, snapshot.nodes[0].name);
    }
}
