//! Synthetic JSON decoder comparison; does not scan or clean user files.
#[path = "../src/snapshot_codec.rs"]
mod production_codec;
use lz4_flex::frame::FrameDecoder;
use ohmydisk_core::{NativePath, Node, NodeKind, Snapshot};
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::Path,
    time::Instant,
};

struct StrictInput<'a>(&'a [u8]);
impl Read for StrictInput<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if !buffer.is_empty() && self.0.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Truncated frame",
            ));
        }
        self.0.read(buffer)
    }
}
struct DigestWriter(Sha256);
impl Write for DigestWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.update(buffer);
        Ok(buffer.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn fingerprint(snapshot: &Snapshot) -> Vec<u8> {
    let mut writer = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut writer, snapshot).unwrap();
    writer.0.finalize().to_vec()
}
fn fixture(count: usize) -> Snapshot {
    Snapshot{id:"snapshot-fixture".into(),scan_id:"scan-fixture".into(),root_id:"0".into(),
        root_path:NativePath::new(Path::new("/fixture/library")),created_at:123,complete:true,cancelled:false,
        nodes:(0..count).map(|index|{
            let path=format!("/fixture/library/Application Support/component/group-{:03}/container/cache/small-file-{:09}.data",index%100,index);
            Node{id:index.to_string(),parent_id:(index!=0).then(||"0".into()),name:format!("small-file-{index:09}.data"),
                path:NativePath::new(Path::new(&path)),kind:if index==0{NodeKind::Directory}else{NodeKind::File},
                logical_bytes:128,allocated_bytes:Some(512),modified_ns:Some(1_700_000_000_000_000_000+index as u64),
                identity:format!("unix:1:{index}"),volume_id:"unix:1".into(),link_count:1,complete:true,is_link_or_reparse:false}
        }).collect(),issues:vec![]}
}
fn encode(source: &Snapshot) -> (Vec<u8>, u64) {
    let encoded = production_codec::encode(source).unwrap();
    (encoded.payload, encoded.json_bytes)
}
fn decode(payload: &[u8], expected: u64, from_str: bool) -> (Snapshot, f64, f64, f64) {
    let total = Instant::now();
    let mut reader = FrameDecoder::new(StrictInput(payload)).take(expected.checked_add(1).unwrap());
    let mut json = Vec::with_capacity(usize::try_from(expected.min(64 * 1024)).unwrap());
    reader.read_to_end(&mut json).unwrap();
    assert_eq!(reader.limit(), 1);
    assert!(reader.into_inner().into_inner().0.is_empty());
    let expand_ms = total.elapsed().as_secs_f64() * 1000.;
    let parse = Instant::now();
    let snapshot: Snapshot = if from_str {
        let text = std::str::from_utf8(&json).unwrap();
        serde_json::from_str(text).unwrap()
    } else {
        serde_json::from_slice(&json).unwrap()
    };
    let parse_ms = parse.elapsed().as_secs_f64() * 1000.;
    drop(json); // Production decode frees its temporary bytes before returning.
    let total_ms = total.elapsed().as_secs_f64() * 1000.;
    (snapshot, expand_ms, parse_ms, total_ms)
}
fn median(values: &[f64]) -> f64 {
    let mut copy = values.to_vec();
    copy.sort_by(f64::total_cmp);
    copy[copy.len() / 2]
}
fn main() {
    if cfg!(debug_assertions) {
        eprintln!("Use a release build to measure snapshot decoding");
        std::process::exit(1);
    }
    let count = 100_000;
    let source = fixture(count);
    let expected = fingerprint(&source);
    let (payload, json_bytes) = encode(&source);
    drop(source);
    let warmups = 2;
    let measured = 7;
    let mut totals = [Vec::new(), Vec::new()];
    let mut expands = [Vec::new(), Vec::new()];
    let mut parses = [Vec::new(), Vec::new()];
    let mut pairs = Vec::new();
    for round in 0..warmups + measured {
        let order = if round % 2 == 0 { [0, 1] } else { [1, 0] };
        let mut pair = [0.; 2];
        for strategy in order {
            let (snapshot, expand, parse, total) = decode(&payload, json_bytes, strategy == 1);
            assert_eq!(snapshot.nodes.len(), count);
            assert_eq!(
                fingerprint(&snapshot),
                expected,
                "Full snapshot field equality failed"
            );
            drop(snapshot);
            if round >= warmups {
                totals[strategy].push(total);
                expands[strategy].push(expand);
                parses[strategy].push(parse);
                pair[strategy] = total;
            }
        }
        if round >= warmups {
            pairs.push(serde_json::json!({"round":round-warmups,"first":if order[0]==0{"from_slice"}else{"utf8_once_from_str"},"slice_ms":pair[0],"str_ms":pair[1]}));
        }
    }
    let result = serde_json::json!({
        "release":true,"architecture":std::env::consts::ARCH,"nodes":count,"fixture":"synthetic_snapshot_all_fields","encoding":"production_streamed_codec",
        "warmups_per_strategy":warmups,"measured_per_strategy":measured,"alternating_order":true,
        "json_bytes":json_bytes,"stored_bytes":payload.len(),"full_snapshot_fingerprint_equal_every_run":true,
        "timing_excludes_fixture_encoding_fingerprint_and_snapshot_drop":true,
        "timing_includes_frame_to_vec_checks_parse_and_temporary_json_drop":true,
        "rss_measured":false,"rss_caveat":"Raw JSON bytes temporarily coexist with the decoded snapshot; any process peak includes fixture generation and warmups.",
        "results":[
            {"strategy":"from_slice","runs_ms":totals[0],"expand_runs_ms":expands[0],"parse_runs_ms":parses[0],"median_ms":median(&totals[0]),"expand_median_ms":median(&expands[0]),"parse_median_ms":median(&parses[0])},
            {"strategy":"utf8_once_from_str","runs_ms":totals[1],"expand_runs_ms":expands[1],"parse_runs_ms":parses[1],"median_ms":median(&totals[1]),"expand_median_ms":median(&expands[1]),"parse_median_ms":median(&parses[1])}
        ],"pairs":pairs
    });
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}
