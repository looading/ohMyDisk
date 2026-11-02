# Auto scan concurrency implementation plan

**Goal:** Measure 4, 8 and available-CPU directory workers on the same fixtures, then select an evidence-based Auto default while retaining explicit worker selection and a shared filesystem budget.

**Architecture:** Keep the existing directory scheduler, bounded batches and single reducer. Put concurrency selection in one core policy used by default scan options and the service. The service owns a process-wide filesystem semaphore; MCP reports both selected per-scan limits and the shared limit.

**Tech stack:** Rust standard library, Tokio, existing Python standard-library benchmark tooling.

**Measured decision:** Auto uses `min(available_parallelism, 4)` and the shared filesystem budget uses `min(available_parallelism, 64)`. Unknown parallelism falls back to 1. Manual integer workers remain a per-scan ceiling of 1–64. The 18-worker small-file trial used about four times the CPU time of four workers while taking longer; directory-heavy trials benefit from a manual setting of eight. See `benchmarks/PERFORMANCE-AUTO-2026-11-02.md` for the full tradeoff and measurement limits.

## Constraints

- Use the existing isolated worktree. Do not touch the other chat's scan-performance checkout.
- Benchmark only owned generated fixtures, in release mode; never clear OS caches or scan a user directory.
- No simultaneous builds or tests during measured trials. Rotate variant order and exclude warmups.
- Validate independent inventory and unchanged fixtures; retain raw samples, binary hash, hardware and timing scope.
- Preserve explicit `workers`, cancellation, bounded filesystem work, platform scope checks and immutable results.
- CPU count is an input, not a speed claim. Do not claim a result on unmeasured Windows/Intel hardware.

## 1. Reproducible three-way measurements

- [x] Extend diagnostic runner and Python argument validation to the existing core range, 1–64, so the local CPU count can be tested.
- [x] Add a three-way harness using the same owned fixture for every variant, balanced order, three warmups and twelve measured samples per variant/case.
- [x] Cover small files (100k), a wide directory (50k), a deep chain (128), mixed files (10k) and two directory-heavy trees.
- [x] Record scan median/p95, CPU time, RSS, correctness and memory limits. Document warm-cache and synthetic-workload limitations.

## 2. Shared Auto policy and service integration

- [x] Select and explain the policy from measured results, including low-CPU fallback and a bounded maximum.
- [x] Use the policy in `ScanOptions::default`, service argument resolution and the global filesystem semaphore.
- [x] Retain manual integer workers. Update MCP schema, capabilities and per-job diagnostic fields together.
- [x] Verify deterministic policy boundaries, invalid arguments, omitted/default selection, explicit selection and cancellation while waiting for the shared budget.
- [x] Verify two scans share one filesystem budget rather than multiplying the per-scan limit.

## 3. Documentation, verification and delivery

- [x] Update current MCP/benchmark documentation and concise English/Chinese README descriptions; keep historical measured reports intact.
- [x] Run Rust workspace tests, format and Clippy checks, Python tooling tests and release build.
- [x] Run an owned-fixture service smoke test for Auto/manual selection and report the actual chosen/global limits.
- [x] Independently review implementation and benchmark methodology; resolve confirmed findings.
- [ ] Delivery: commit and push to existing PR #6; record pushed-head native CI results in the PR description.
