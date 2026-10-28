# Published scanner reference runner

This independent Rust crate benchmarks the published ohMyDisk v0.1.0 scanner and
query implementation. Both `ohmydisk-core` and `ohmydisk-platform` are Git
dependencies pinned to the peeled release commit
[`37098869488fd0607fb13bb32a1298e6d5ed9006`](https://github.com/looading/ohMyDisk/commit/37098869488fd0607fb13bb32a1298e6d5ed9006).
The crate has its own `[workspace]`, so it cannot accidentally use the optimized
path dependencies in the surrounding checkout. It builds core/platform code
without the application or iced GUI.

The original benchmark runner was an uncommitted diagnostic example added after
the release, and its original source was not preserved. `src/main.rs` is a
reconstructed, release-compatible diagnostic wrapper. It calls the unchanged
published `scan::scan`, `NativePlatform` and `query::query` APIs. It has no
`DirectoryEvent`, custom `read_directory`, bulk enumeration or `QueryIndex`
implementation. The scanner and native filesystem behavior come from the pinned
release source, rather than a portable mode in the optimized scanner.

The measurements match the comparison contract: metadata calls and summed
metadata duration, scan duration and progress callback count, snapshot
serialization duration/bytes, first legacy children and descendants query
duration, cancellation latency, OS process high-water RSS read immediately when
the scan returns, and structural validation digest v1. Serialization, validation
hashing and queries are outside `scan_ms`. Query index construction and directory
visitor instrumentation did not exist in the old API, so their corresponding
optional fields are absent, not fabricated as measured zeroes.

## Build and compare

Use Rust 1.88 or newer. Run from the repository root; no application checkout or
manifest changes are needed:

```sh
cargo build --locked --release \
  --manifest-path benchmarks/reference/Cargo.toml \
  --target-dir target/reference

cargo build --release -p ohmydisk-platform --example scan-bench

python3 benchmarks/scan_bench.py \
  --binary target/release/examples/scan-bench \
  --reference-binary target/reference/release/ohmydisk-scan-reference \
  --reference-enumeration default \
  --output docs/scan-reproduced-release-comparison.json
```

On Windows use the `.exe` suffix for both binaries. Reference enumeration must
remain `default`: the legacy runner does not accept `--portable-enumeration`.
To compare the traversal algorithm while excluding the new candidate's native
bulk enumeration, add `--enumeration portable` for the candidate only. To measure
the new default implementation against the published release, leave candidate
enumeration at `default`.

The included `benchmarks/reference/Cargo.lock` freezes registry dependencies as
well as the Git source revision. Use `--locked` to retain that resolution. The
direct JSON, SHA-256 and Unix libc versions match the published release
lockfile. Record the lockfile and Rust compiler version with new comparisons;
compiler, dependency, target and OS differences can affect timings.
Use the same compiler, target and release optimization settings for both sides.
The isolated reference target directory prevents build artifacts from being
shared with the current scanner.

For a quick correctness check before a full run, add
`--fixture small-files --small-files 25 --iterations 1 --warmups 0
--cancel-iterations 0`. The harness checks counts and logical bytes independently
and requires matching structural digests across complete scans on the same
fixture. A matching digest validates the projected snapshot structure; it does
not establish compiler output or binary identity.

## Original recorded baseline provenance

The original local baseline results used a preserved executable named
`scan-bench-v0.1.0`, built against the release source commit above. Its SHA-256 is:

```text
bba56ff7f61761dfd1233bde3e4cf08f42cc7946c626b305ce4859419666ad21
```

That hash identifies the executable used in the already recorded measurements.
It is not an expected hash for a build of this reconstructed wrapper. The
original diagnostic source and complete original build inputs are unavailable;
this crate therefore provides a new, source-reproducible measurement of the
published scanner, without claiming the rebuilt executable or timings are
identical to the frozen local baseline. New reports must identify their actual
reference binary hash and retain the original report's hash when citing the
original measurements.

Digest v1 is kept verbatim from the current diagnostic wrapper: root-relative
native paths are encoded by `NativePath`, node tuples sort by the encoded path,
and the compact JSON hash contains path, kind, parent path, logical/allocated
bytes, completeness, link/reparse status, link count, relative issue paths/codes,
and snapshot complete/cancelled flags. UUIDs, timestamps and absolute root paths
are excluded. This contract permits comparison with both preserved baseline
results and the optimized runner when they scan the same fixture.
