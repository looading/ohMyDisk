# Scan Performance Implementation Plan

> **For agentic workers:** Use executing-plans to implement the measured changes task-by-task. Preserve scan and cleanup semantics; benchmark before and after each round.

**Goal:** Improve real scanning and first-result latency with reproducible release benchmarks and semantic parity checks.

**Architecture:** Measure the current core scanner separately from service persistence and first-page queries. Optimize allocations and progress work first, then test native bulk metadata enumeration and bounded immutable snapshot reuse. Keep fallback enumeration and durable cleanup records.

**Tech Stack:** Rust, Python 3.11+, native filesystem metadata, existing SQLite service, LZ4 frame snapshot storage.

## Global Constraints

- Preserve hidden files, logical and allocated sizes, hardlink accounting, native paths, permission errors, symlink and cross-volume boundaries.
- Never reduce durability of cleanup ownership, started records or item outcomes.
- Default to temporary deterministic fixtures; explicit real roots are read-only and their paths never enter published reports.
- Record warm-cache measurements honestly; do not purge caches or claim cold-cache behavior.
- Compare medians and tail values across repeated runs, with identical fixture inventories and canonical semantic digests.

## Task 1: Baseline feedback loop

- [x] Add `crates/ohmydisk-platform/examples/scan-bench.rs`, a release runner calling the unchanged scanner with instrumented metadata and a noop progress callback.
- [x] Add `benchmarks/scan_bench.py` for small files, a wide directory, a deep tree and mixed special entries. Separate fixture setup from measured runtime.
- [x] Save an immutable baseline runner in a temporary directory and measure scan, serialization, query, memory and cancellation.
- [x] Measure service job completion, first children and first descendants on the same fixture; preserve a reference app binary.

## Task 2: Remove measured redundant work

- [x] Remove full Node clones and the string-keyed aggregation map by retaining parent indices during traversal. Confirm identical digests.
- [x] Bound noncritical progress persistence frequency without changing terminal job or cleanup durability. Compare service/raw overhead.
- [x] Re-run all fixtures and keep only gains larger than normal measurement noise.

## Task 3: Native metadata experiment

- [x] Introduce cancellable directory enumeration with a default portable implementation and optional metadata records.
- [x] On macOS, evaluate getattrlistbulk for regular files with stat fallback for directories, links, unsupported fields and filesystems.
- [x] Prove parity for allocation, resource forks, compression, sparse files, hardlinks, native names, timestamps and errors before enabling the path.
- [x] Record raw improvements independently from whole-service improvements; do not infer Windows speedups from macOS.

## Task 4: Result availability and regressions

- [x] Avoid duplicate full-snapshot decoding and repeated per-request tree construction with a bounded immutable query view.
- [x] Keep snapshot and cursor identity stable, release cached data with scan lifecycle, and test restart/cache misses.
- [x] Provide reproducible before/after JSON, commands, environment limits and a manual native benchmark workflow for future comparisons.
- [ ] Run workspace tests, actual MCP smoke and native CI. Independently review safety and benchmark validity before publishing.

## Iteration criterion

Run multiple distinct rounds. Reject regressions and optimizations that change semantics. When the next measured change falls within noise or requires a separate platform experiment, report that boundary and leave the benchmark runnable for subsequent work.

## Measurement notes

- Parent-index aggregation alone delivered small, inconsistent latency gains; it removed full Node clones and reduced retained allocations.
- Streaming portable enumeration removed whole-directory buffering/sorting. Directory metadata must be refreshed after queue wait immediately before traversal; a deterministic volume-change regression test guards that window.
- macOS bulk regular-file metadata improved raw scan throughput; directories, links and unavailable fields retain stat fallback.
- Throttling noncritical progress to 250 ms did not materially improve terminal latency: snapshot JSON persistence dominated. FULL durability was preserved.
- Streaming LZ4 snapshot writes cut persistence and disk footprint. Legacy JSON rows remain readable and untouched; corruption/unsupported codecs fail closed.
- An initial compressed streaming JSON decoder regressed cold reads. Slice decoding, immutable query views and canonical-ID indexing were measured separately to address that regression.
- Final evidence and exact reproduction commands live in `benchmarks/RESULTS.md`. No cold-filesystem or whole-user-directory timing claim is made from warm generated fixtures.
