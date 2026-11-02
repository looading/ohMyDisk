# Scan progress verification

Scan percentages are explicitly estimated from discovered/resolved directory tasks, with a monotonic high-water mark through 90 percent. Saving and indexing reserve checkpoints at 95 and 98. Only a complete, stored, indexed and published result reaches 100. Partial results, cancellation, interruption and failure retain a sub-100 estimate. Missing optional telemetry in older data stays unknown.

## Isolated source and local checks

The final source is in the dedicated `codex/scan-progress-ui` worktree, based on `a7b3abf`. Only this task's changes were moved there after unrelated storage changes appeared in the original shared worktree. The original workspace retains its separate storage work. Tests below use the independent source, with unchanged `service/Cargo.toml`, `store.rs` and `snapshot_codec.rs`.

| Check | Result |
|---|---|
| `cargo fmt --all --check` and `git diff --check` | Passed |
| `MACOSX_DEPLOYMENT_TARGET=13.0 cargo test --locked --workspace` | 257 passed, zero failed |
| `cargo clippy --locked --workspace --all-targets` | Completed; lint warnings remain |
| `python3 -m unittest discover -s tests -p 'test_*.py' -v` | 21 passed, zero failed |
| Release application and diagnostic runner builds | Passed |
| Real stdio MCP smoke using an isolated foreground backend | Passed, including success-only 100, preserved summary telemetry and Auto/manual worker limits |
| macOS packaging and strict bundle signature verification | Passed |

Core tests cover optional legacy serialization, expanding discovery without regression, saturating counters, rejected/error tasks, cancellation and worker failure. They prove exactly one enumeration per directory across native, portable and untyped enumeration models. Service tests cover publication checkpoints, canceled estimates, complete versus partial results, root failure, summary read skew and shared-budget cancellation. GUI tests cover live raw data and language switching, job identities, stale terminal caches, cleanup refresh, legacy records, narrow layout and read-only reconnection without navigation/selection changes.

## End-to-end progress samples

- [40k-directory live service trace](../../benchmarks/results/2026-11-02/scan-progress/live-service.json): 40,001 directory tasks, real MCP polling through 0, incremental traversal estimates, 95, 98 and 100; total 9,318 ms. The marker file was unchanged.
- [140k-directory live service trace](../../benchmarks/results/2026-11-02/scan-progress/live-service-large.json): 140,001 directory tasks, all estimates 0 through 89 observed, then 95, 98 and 100; total 27,143 ms. The marker file was unchanged.

Both datasets were generated solely for acceptance. A single worker intentionally extended the capture window; these are functional service traces rather than comparisons of default scan speed. No user directory was scanned by the tests.

## Native UI

An isolated Chinese-language application rendered the owned fixture's completed `100%` and retained total duration. On a later running scan, the native source card visibly rendered `估算 18%` and a partially filled scan bar, independently from the disk-capacity bars. The existing view-results action remained identified as previous results while the new attempt was running.

That startup observation exposed a missing reconnect path: an already-running backend scan needed to be attached to the GUI's active-job model to keep polling. The final implementation now attaches the latest eligible job only on an accepted catalog refresh/reopen, reuses the existing 650 ms active subscription, and preserves navigation and collection state. Four regression tests cover the guards, stale responses, legacy data and completion handoff. At the time this note was recorded, the final native reconnect replay was blocked by macOS lock; later desktop and native CI results are recorded on the pull request.

## Telemetry cost

[Paired overhead report](../../benchmarks/SCAN_PROGRESS_2026-11-02.md) and its raw samples compare the exact pre-progress baseline at the same four-worker setting. All 36 scans passed inventory, structural/occupancy fingerprints and unchanged-fixture checks, with identical instrumented filesystem-method counts. Median scan times changed by approximately +0.6% for small files and +0.2% for directory-heavy data, within this warm-cache desktop experiment's timing noise. No claim is made about exact overhead, cold disks, real-home directories or Windows performance.
