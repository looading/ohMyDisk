# Scan performance, 2026-10-28

The v0.1.1 changes reduce both macOS traversal time and the time until a scan can
be queried. The final 100,000-file service comparison completed in a median
352 ms versus 1,000 ms for v0.1.0. Fresh first-page queries took 9–10 ms versus
75–80 ms. A historical first children page remains about 6 ms slower than the
old version; repeated historical queries are much faster. That tradeoff is
included below rather than treating every metric as an improvement.

## Method and limits

Local measurements used an Apple M5 Pro, 18 logical CPUs, arm64 Darwin 25.6.0,
APFS and Rust 1.94.1, with the release profile and thin LTO. The reports record
binary SHA-256 values, compiler/OS metadata, raw trials, fixture parameters and
validation state. Each final comparison used one excluded warmup and seven
measured runs per variant in alternating order. p95 is nearest rank; with seven
samples it is the largest sample. Core and service measurements ran separately,
without concurrent compilation or another benchmark.

These are **warm filesystem-cache measurements on generated fixtures**, not
measurements of a real whole disk or home directory. Fixture creation and
independent inventory touch filesystem caches. No caches were cleared. Permission
prompts, directory distribution, external disks and active filesystem changes can
produce different results. No macOS timing is presented as a Windows speedup.

## Core traversal

The current runner is compared with a separately built runner using the actual
published v0.1.0 core/platform source at
[`3709886`](https://github.com/looading/ohMyDisk/commit/37098869488fd0607fb13bb32a1298e6d5ed9006).
The [independent reference crate](reference/README.md) pins those Git dependencies
and has its own lockfile. Its diagnostic wrapper was reconstructed; it does not
claim binary identity with the earlier frozen diagnostic baseline.

| Fixture | v0.1.0 median / p95 ms | Current median / p95 ms | Median time reduction |
| --- | ---: | ---: | ---: |
| 100,000 × 128-byte files, 1,000 per directory | 301.60 / 318.09 | 152.02 / 160.46 | 49.6% |
| One directory with 50,000 × 32-byte files | 175.06 / 181.90 | 70.77 / 72.65 | 59.6% |
| 128 levels × 64 files, 8,192 files total | 48.80 / 50.81 | 33.19 / 34.22 | 32.0% |
| 10,000 mixed files plus hidden/link fixtures | 36.34 / 39.26 | 25.47 / 27.90 | 29.9% |

All four fixtures passed independent inventory checks and matching canonical
snapshot digests across both runners. Mixed fixtures deliberately remain
incomplete because symlinks are boundary nodes. The digest compares relative
native paths, parent relationships, kind, logical/allocated sizes, completeness,
link flags/counts and issue codes; it excludes opaque node IDs and timestamps.
Native metadata field parity is covered separately by platform tests.

For 100,000 files, explicit `Platform::metadata` calls fell from 100,102 to 102.
This is **not a total syscall count**: native directory enumeration obtains
regular-file metadata through bulk calls. Directories, links and unavailable
attributes still use metadata fallback. Scan-time process peak RSS was about
72.6 MiB versus 68.1 MiB; for the wide fixture it was 52.8 versus 37.7 MiB.
This peak includes startup and scanning, and is captured before serialization,
index construction, queries and validation hashing.

An earlier same-binary [default/portable comparison](results/2026-10-28/core-native-vs-portable.json)
isolated the macOS enumerator: 100,000-file medians were 158.83 versus 237.82 ms,
and wide-directory medians were 70.64 versus 128.27 ms. Both modes use the new
traversal. Windows currently uses the portable enumerator in both modes.

The earlier frozen-reference cancellation probe on the 50,000-file wide
directory observed median cancellation latency of 52.61 ms for the old scanner
and 0.97 ms for the streaming scanner, three observed cancellations per variant.
This is core cancellation latency; saving/indexing and IPC completion are outside
that metric. Small fixtures that finish without observing a timer request do not
establish a cancellation guarantee.

Raw evidence: [final source-rebuilt reference comparison](results/2026-10-28/core-rebuilt-vs-v0.1.0.json),
[earlier frozen-reference comparison and cancellation](results/2026-10-28/core-vs-v0.1.0.json).

## Service completion and queries

Both local release application binaries scan the same generated 100,000-file
fixture. RPC timings include diagnostic client startup; terminal time also
includes polling. The fresh snapshot is persisted and indexed before the job
reports terminal completion. Historical children and descendants each use a
separately restarted foreground service against the saved state, with no rescan
or preceding snapshot request. OS caches remain warm.

| Metric | v0.1.0 median / p95 ms | v0.1.1 median / p95 ms |
| --- | ---: | ---: |
| Scan request through terminal observation | 999.82 / 1099.81 | 352.13 / 369.50 |
| Fresh first children page, limit 100 | 74.82 / 76.80 | 8.65 / 10.57 |
| Fresh first descendants page, limit 500 | 79.84 / 81.97 | 10.21 / 11.33 |
| Historical cold children page | 73.20 / 81.18 | 79.12 / 81.15 |
| Historical cold descendants page | 85.13 / 90.71 | 79.24 / 86.73 |
| Historical repeated children page | 72.69 / 78.07 | 9.09 / 11.40 |
| Historical repeated descendants page | 84.26 / 87.36 | 10.73 / 11.27 |

The historical first children page is still 5.92 ms slower at the median, about
8.1%. It pays decoding and index construction once. Its p95 is approximately
unchanged, and later pages reuse the immutable view. The first descendants page
is slightly faster. Full page values, order, cursor and scan summary are checked
for equality before/after restart for every measured trial.

In the final candidate, source-reported medians were 132.53 ms scanning,
143.78 ms persistence, and 8.14 ms index construction. The remainder of the
terminal-observation time includes other durable records, IPC and polling;
stage medians should not be added to infer an exact per-trial decomposition.

| Resource metric, median | v0.1.0 | v0.1.1 |
| --- | ---: | ---: |
| Maximum sampled service RSS during fresh scan/queries | 279.8 MiB | 98.1 MiB |
| State database + WAL + SHM while service is alive | 113.5 MiB | 9.6 MiB |

Service RSS is sampled from `ps`, **not a process high-water mark**. Historical
decode processes have their own RSS measurements in the raw report. Decoding
temporarily retains both raw JSON and a constructed Snapshot. Database size
includes WAL and does not imply those files are compacted after every release.

Raw evidence: [final service comparison and historical validation](results/2026-10-28/service-final-vs-v0.1.0.json).

## Measured iterations

| Experiment | Finding and retained change |
| --- | --- |
| Parent-index aggregation | Removed full Node clones and the string-keyed aggregation map. Initial latency gains were small or within noise; this alone did not explain the slow application. |
| Streaming enumeration | Removed full-directory buffering/sorting. A queued directory must receive its metadata check immediately before traversal; a deterministic volume-change regression test guards this behavior. |
| macOS bulk metadata | Obtains regular-file attributes with `getattrlistbulk`, retaining portable enumeration and per-entry fallback. The final core comparisons establish the combined traversal benefit. |
| Progress throttling | Limits noncritical callback writes to 250 ms, first/last always. A paired terminal comparison of 889.95 versus 871.56 ms showed no meaningful win; measured snapshot persistence was about 669 ms and dominated. |
| Streaming LZ4 snapshots | Cut persistence, stored bytes and fresh-scan memory. An early implementation completed in 370.76 versus 821.75 ms but regressed first queries to about 285 ms, so that decoder was not retained. |
| Contiguous decode and immutable query views | Avoids repeated full snapshot parsing and per-query tree reconstruction. Fresh first pages reached about 9–10 ms, but cold historical children initially took 96.96 versus 74.70 ms. |
| Checked canonical position IDs | Bypasses hashing and ID sorting only when every node ID equals its canonical numeric position. Index construction fell from about 21 to 11 ms; arbitrary and reordered IDs retain the general path. |
| Decimal lexicographic order and UTF-8 validation | Generate canonical ID string order in O(n), then stable-sort by size, preserving exact tie order. Validate contiguous JSON UTF-8 once before `from_str`. Final index construction was about 8 ms and historical cold children about 79 ms. |

Intermediate reports identify the actual diagnostic binaries; they are evidence
of the development experiments, not separate released versions:
[progress](results/2026-10-28/iteration-progress.json),
[compression and the initial query regression](results/2026-10-28/iteration-compression.json),
[initial historical cache load](results/2026-10-28/service-history-vs-v0.1.0.json),
[canonical-ID iteration](results/2026-10-28/iteration-dense-index.json).

The decoder change also has a reproducible synthetic microbenchmark:

```sh
cargo run --locked --release -p ohmydisk-service --example decode-bench > decode.json
```

It constructs 100,000 nodes, uses the production encoder and verifies a complete
Snapshot fingerprint after every decode. The reproduced example measured about
56.6 ms for `from_slice` and 53.3 ms for UTF-8-once plus `from_str`; this is a
decoder experiment, not an equivalent percentage change in application latency.
See [independent experiment](results/2026-10-28/iteration-decode-utf8.json) and
[reproduced example](results/2026-10-28/decode-final-reproduced.json).

## Reproduce and validate

The [complete native workflow](https://github.com/looading/ohMyDisk/actions/runs/40717898553)
also passed on Windows x64, macOS Intel and macOS ARM. Its
[per-platform raw reports and summary](results/native-v0.1.1/README.md) are retained
in the repository. Those runs use five measured samples and a 64-level deep tree;
the core reference is the same current binary in portable mode, rather than
v0.1.0, and service measurements are current-only. They validate the cross-platform
benchmark flow and must not be substituted for the local old/new tables above.

```sh
cargo build --locked --release -p ohmydisk-platform --example scan-bench
cargo build --locked --release --manifest-path benchmarks/reference/Cargo.toml --target-dir target/reference
python3 benchmarks/scan_bench.py \
  --binary target/release/examples/scan-bench \
  --reference-binary target/reference/release/ohmydisk-scan-reference \
  --iterations 7 --deep-files-per-level 64 --output core.json

cargo build --locked --release -p ohmydisk
python3 benchmarks/service_bench.py \
  --binary target/release/ohmydisk \
  --reference-binary /path/to/preserved/v0.1.0/ohmydisk \
  --runs 7 --historical-restart --output service.json
```

Use matching architecture, compiler and release settings for application
binaries; the old service must support `service --stay-alive`, as v0.1.0 does.
Windows executables need the `.exe` suffix. Default fixtures are temporary and
the harnesses never invoke cleanup. The [core](SCAN_BENCH.md) and
[service](SERVICE_BENCH.md) guides define every timing and explicit-root rule.
The [manual native workflow](BENCHMARK_CI.md) supplies repeatable Windows/macOS
measurements; the normal build workflow runs small correctness smoke fixtures.
Timing thresholds are informational, while inventory, digest and response
validation failures fail the run.

Local validation passed 66 Rust tests, six packaging tests, formatting, Clippy
(existing warnings remain), and actual MCP smoke in both automatic and supervised
service modes, including native trash, permanent deletion, mode isolation,
idempotency, reconnect and refresh. [Native Windows/macOS CI](https://github.com/looading/ohMyDisk/actions/runs/40716139549)
passed all three platform jobs before PR #1 merged; release CI gates distribution
of all three platform packages. Only CI-built executables are intended for public releases;
local Cargo builds can contain developer paths in compiled panic locations.

Snapshot encoding has a versioned, checked frame and length. Legacy JSON rows
remain readable without rewriting; unknown codecs or corrupt new payloads fail
instead of falling back silently. SQLite FULL durability and cleanup
Started/outcome records are unchanged. Native names, hidden entries, hardlinks,
sparse/compressed/resource-fork files, link boundaries, directory volume changes,
cancellation and pagination are covered by tests and differential comparisons.

The query cache retains up to two immutable Snapshot/index pairs within an
estimated 128 MiB budget, except that one newest oversized view is retained alone.
In-flight readers can outlive eviction. This is not a hard RSS ceiling, and the
full Snapshot still uses O(number of nodes) memory. Million-node and truly cold
whole-volume scans remain separate measurements; these fixture results do not
establish their latency or memory limits.
