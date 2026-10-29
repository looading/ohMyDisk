# Occupied Space and Concurrent Scan Implementation Plan

> For agentic workers: implement the approved design task by task using isolated
> file ownership. The user explicitly chose multi-agent execution; no second
> execution-choice approval is required.

**Goal:** Make default capacity views reconcile with the selected filesystem,
ship a DaisyDisk-style iced GUI and measured bounded directory concurrency.

**Architecture:** A native provider supplies coherent capacity observations and
per-file attribution. One reducer builds immutable metric-aware snapshots while
bounded Tokio blocking workers enumerate directories. The service and GUI consume
the same occupancy fields, with logical metadata retained for legacy consumers.

**Tech Stack:** Rust 2024 / minimum 1.88, iced 0.14, existing Tokio, libc and
windows-sys; existing SQLite FULL + LZ4 snapshot persistence and stdio MCP.

## Global constraints

- macOS and Windows remain native supported targets; no admin or raw-device access.
- Capacity is OS-observed, not a literal 1 TB cap. Never truncate raw data or
  proportionally scale file bytes to force an apparently balanced graph.
- Keep logical and reported allocation fields; optional new fields use serde
  defaults and omit absent fields so old plan serialization/cursors remain valid.
- Unknown, known zero and partial measurements are distinct. Shared/system/
  unscanned space is explanatory and never a cleanup target.
- Preserve no-link traversal, same-volume checks, directory-identity admission,
  fresh queued-directory checks, explicit cleanup authorization and durability.
- No user jobs are stopped; no home/root scan is run for benchmarking.

## Task 1: Shared space contract and metric queries — root

Files: core/src/model.rs, core/src/space.rs, core/src/query.rs,
core/src/query_index.rs, core/src/platform.rs, and affected fixtures.

Interfaces:

```rust
// serde snake_case, Logical is the legacy default and omitted from Query JSON.
enum SizeMetric { Logical, Allocated, Attributed }
enum AttributionProvider { MacPrivateSize, NtfsUnsharedAllocation, Unsupported }
struct NodeSpace { attributed_bytes: u64, measured_entries: u64, unmeasured_entries: u64 }
struct SpaceObservation {
    volume_id: String, total_bytes: u64, available_bytes: u64,
    captured_at: u64, source: String, capacity_scope: String,
    provider: AttributionProvider,
    used_bytes: Option<u64>, caller_available_bytes: Option<u64>,
}
struct SpaceAccounting {
    before: SpaceObservation, after: Option<SpaceObservation>,
    attributed_bytes: u64, shared_unattributed_bytes: Option<u64>,
    reconciliation_status: String, measurement_status: String,
}
// FileInfo.private_bytes: Option<u64>
// Node.space_usage: Option<NodeSpace>
// Snapshot.space_accounting: Option<SpaceAccounting>
// Volume.space_observation: Option<SpaceObservation>
// Platform::scan_metadata defaults to metadata.
// Platform::space_observation defaults to Ok(None).
```

- [ ] Preserve an immutable v0.1.2 runner/application baseline outside the repo.
- [ ] Add optional/default/skip-none fields and a metric-aware Query with default
  Logical omitted from serialization. Keep existing logical cursor signatures.
- [ ] Add pure attribution/aggregation/reconciliation tests: C=995,A=245,E=500
  gives R=250; E greater than used returns inconsistent with no fake remainder;
  hardlinks/empty identity/unsupported providers remain unmeasured.
- [ ] Implement metric extraction, full/partial/unknown state, consistent sorting
  and numeric filtering in reference and indexed query paths.
- [ ] Verify differential pages/cursors for every metric and old JSON snapshots.

## Task 2: Native attribution and capacity providers — space agent

Files owned: platform/src/lib.rs, macos_bulk.rs, windows.rs and a new native
space-provider module; platform-only tests. Do not edit shared core models.

Consumes the Task 1 interfaces. Produces ordinary FileInfo with private bytes,
coherent SpaceObservation and Volume.space_observation when available.

- [ ] Add per-record returned-bit checked macOS private size, keep all existing
  native metadata fields and unsupported/partial fallback behavior.
- [ ] Implement same-basis macOS native volume observation with stat fallback;
  include optional used detail, never put purgeable capacity into raw available.
- [ ] Implement Windows actual allocation-unit capacity and caller detail; probe
  local NTFS/shared-block capability once for the scan context.
- [ ] Keep metadata cheap for cleanup; scan_metadata is the optional extra probe.
- [ ] Test fresh files, sparse files, clone/shared/unsupported values, malformed
  records, capacity arithmetic and ordinary FileInfo parity.

## Task 3: Bounded Tokio scan workers — async agent

Files owned: core/src/scan.rs, a new core/src/scan_async.rs, core/Cargo.toml,
core/src/platform.rs kind hints if needed, core traversal/concurrency tests.
Coordinate the platform trait addition with root before editing it.

Consumes the space types and platform provider. Produces:

```rust
struct ScanOptions { workers: usize, batch_entries: usize, batch_bytes: usize }
async fn scan_async(
    root: PathBuf, platform: Arc<dyn Platform>, cancelled: Arc<AtomicBool>,
    options: ScanOptions, filesystem_budget: Arc<tokio::sync::Semaphore>,
    progress: tokio::sync::watch::Sender<ScanProgress>,
) -> Result<Snapshot, AppError>;
```

- [ ] Keep the serial scan entry point as a reference and populate new metrics.
- [ ] Add a single-reducer, directory-level worker protocol with admission before
  reading, streaming bounded batches and parent-done child eligibility.
- [ ] Propagate NodeSpace and before/after space observations in both paths.
- [ ] Stop dispatch on cancel/error, unblock bounded sends/replies and await every
  worker before returning. Preserve partial snapshot and issue semantics.
- [ ] Add deterministic alias/volume-change/full-channel cancellation/panic tests.
- [ ] Expose worker count to the benchmark runner through a coordinated change;
  no worker-per-file spawning or unbounded event buffers.

## Task 4: DaisyDisk-style iced views — GUI agent

Files owned: crates/ohmydisk/src/gui/**. New focused modules may split the large
mod.rs; keep application messages and service interface integration clear.

Consumes Node.space_usage, Snapshot/scan summary.space_accounting,
Volume.space_observation and Query size_metric="attributed".

- [ ] Implement disk overview and analysis as two views with explicit scan/open/
  rescan actions; identify related APFS entry points without summing capacities.
- [ ] Make the chart the main workspace, add distinct inherited branch colours,
  compact directory rows and a collapsed expandable collector.
- [ ] Add epoch-safe debounced hover child preview and centre-click parent return.
- [ ] Render capacity/free/attributed/remainder as one coherent root view; show
  known zero/shared/unmeasured distinctly and never use raw logical as a fallback.
- [ ] Keep unknown/synthetic space navigable for explanation but non-collectable.
- [ ] Preserve plan review and independent trash/permanent confirmation.
- [ ] Validate with isolated state/fixtures and the existing native app controls.

## Task 5: Service integration and query concurrency — root

Files: service/src/engine.rs, snapshot_cache.rs, core query modules as needed,
service lifecycle tests, mcp documentation/schema and scan/service benchmarks.

- [ ] Route scan jobs into the async coordinator with existing job lifecycle and
  a shared filesystem budget; keep terminal durability and worker cleanup.
- [ ] Persist/return SpaceAccounting in scan summaries without reading all nodes.
- [ ] Add metric capability/status metadata while retaining legacy raw fields.
- [ ] Limit expensive queries independently and implement per-snapshot cold-load
  single-flight without holding the ready-cache mutex while decoding/indexing.
- [ ] Test old storage, new storage, release/restart/cursors, concurrent queries,
  cancellation responsiveness and unchanged cleanup authorization.

## Task 6: Benchmark, review and distribution — root/team

- [ ] Run release worker 1/2/4/8 comparisons on small/wide/deep/mixed/directory-
  dense fixtures. Record semantic parity, latency median/p95, RSS and cancellation.
- [ ] Measure physical-provider overhead separately from concurrency gains.
- [ ] Select a measured default; retain serial/one-worker fallback when a device
  does not benefit. Do not claim constant snapshot/frontier memory.
- [ ] Run cargo fmt, workspace tests, Clippy and Python/MCP smoke, then independent
  code review of native FFI, cancellation/admission and occupancy reconciliation.
- [ ] Verify the isolated GUI layout/interactions, publish native Windows/macOS
  CI builds only, validate checksums/privacy markers, and record actual evidence.

## Test commands

```sh
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets
cargo fmt --all --check
python3 -m unittest discover -s tests -p 'test_*.py' -v
python3 tests/mcp_smoke.py --trash --service-mode supervised
```

Expected: no failing tests or compiler errors; existing lint warnings are tracked
separately. Timing comparisons are reported only after semantic checks pass.
