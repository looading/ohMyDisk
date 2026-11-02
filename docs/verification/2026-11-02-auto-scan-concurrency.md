# Auto scan concurrency verification

The selected policy is Auto `min(available CPU parallelism, 4)`, with one service-wide filesystem budget of `min(available CPU parallelism, 64)`. CPU detection failure uses one. Manual integer `workers` from 1 through 64 is a per-scan ceiling and remains subject to the shared budget.

## Measurement evidence

- [Full performance report](../../benchmarks/PERFORMANCE-AUTO-2026-11-02.md)
- [Raw 4/8/CPU trials](../../benchmarks/results/2026-11-02/worker-matrix/three-way.json)
- [Numeric summary with raw-report SHA-256](../../benchmarks/results/2026-11-02/worker-matrix/summary.json)

Six owned synthetic fixtures, three warmups and twelve measured trials for each of 4, 8 and 18 workers: 270 scans passed independent inventory, matching structural fingerprints and unchanged-fixture checks. Occupancy fingerprints were also stable; owned fixtures were removed and the frozen measured binary did not change. Results are warm-cache core measurements on a running macOS desktop, not a real-home, cold-disk, Windows or simultaneous-service throughput claim.

## Local checks

| Check | Result |
|---|---|
| `cargo fmt --all --check` and `git diff --check` | Passed |
| `MACOSX_DEPLOYMENT_TARGET=13.0 cargo test --locked --workspace` | 231 passed, zero failed |
| `cargo test -p ohmydisk-platform --example scan-bench` | One diagnostic-runner test passed |
| `python3 -m unittest discover -s tests -p 'test_*.py' -v` | 21 passed, zero failed |
| `MACOSX_DEPLOYMENT_TARGET=13.0 cargo clippy --locked --workspace --all-targets` | Completed; lint warnings remain |
| `MACOSX_DEPLOYMENT_TARGET=13.0 cargo build --locked --release -p ohmydisk` | Passed |
| `python3 tests/mcp_smoke.py target/release/ohmydisk --service-mode supervised` | Passed against an isolated foreground backend |
| `python3 scripts/package.py --skip-build` and strict macOS bundle signature verification | Passed |

The smoke test observed 18 available CPUs, Auto 4, maximum manual workers 64 and a shared budget of 18. It checked omitted-worker Auto, idempotent manual 64, the effective ceiling of 18, querying, immutable cleanup plans and refresh. Destructive smoke operations used only test-created temporary files; no user directory was scanned.

Policy tests cover zero/unknown and 1/2/4/8/18/64/128 CPU counts, saturating bounds, manual values above Auto and invalid inputs. Service tests verify persisted queued/final settings and that both Auto and manual jobs cannot enter the filesystem while their shared budget is held; existing core tests check actual concurrent scans obey one shared budget. Independent source and methodology review found no blocking issue.

Native Windows/macOS CI results for the pushed commit are recorded in [PR #6](https://github.com/looading/ohMyDisk/pull/6). An already-running older backend must restart before clients receive the new policy. No production GUI session was restarted during this work.
