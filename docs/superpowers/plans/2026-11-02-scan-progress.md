# Scan progress bar and percentage

**Goal:** Show a live scan progress bar and percentage in both overview and details, retain completion status and total duration, and expose the same telemetry to MCP.

**Architecture:** Extend `ScanProgress` with optional completion telemetry, leaving old records readable. A single reducer counts discovered and resolved directory tasks. Its monotonic known-work estimate advances through at most 90 percent of the overall job, then service-owned saving/indexing checkpoints advance to 95/98. Only a fully successful durable scan reaches 100. The GUI renders the shared data with runtime English/Chinese text.

**Design decision:** A first walk cannot know undiscovered subtrees. Label active numeric progress as an estimate. The known-work high-water estimate may pause while new directories are found; it is neither elapsed-time progress nor an exact fraction of all files. No prewalk, timer-driven fake growth, bytes/capacity ratio, or history load is introduced.

**Contract:** `ScanProgress.completion: Option<ScanCompletion>`; `ScanCompletion` has `percentage: u8`, `estimated: bool`, `basis: String`, `discovered_directory_tasks: u64`, `completed_directory_tasks: u64`. Missing completion means unknown. Initial queued scans have zero estimated progress. Active scanning uses basis `discovered_directory_tasks`; service phases use `phase_checkpoint`; full success uses `finished` with `estimated=false`. Cancellation, failure, interruption and partial results keep their last sub-100 estimate. Scan summaries include optional raw job progress for completion display.

**Constraints:** Dedicated managed worktree on `codex/scan-progress-ui`, based on main commit `a7b3abf`; only this task's edits were moved after concurrent storage edits appeared in the previous shared worktree. Other work remains in its source workspace. Do not stop a user's live scan or scan a user directory for acceptance. Preserve Auto workers, scope checks, result immutability, cleanup behavior and 250 ms progress throttling. Do not animate unknown progress using another timer. Existing source capacity bars keep their capacity meaning.

## Backend

- [x] Add defaulted optional completion telemetry and a reusable monotonic estimator with bounded integer arithmetic.
- [x] Count each discovered directory task and each resolved/rejected task once in asynchronous and synchronous traversal; preserve cancellation partial counters.
- [x] Publish a reducing phase before aggregation and guarantee the final watch update is retained.
- [x] Persist zero/queued progress, saving/indexing checkpoints and success-only 100 percent. Retain original terminal phase semantics for non-scan jobs.
- [x] Add progress to scan summaries and document percentage semantics for MCP clients.
- [x] Test legacy deserialization, expanding discovery, concurrent and rejected tasks, cancellation, partial/error terminal states and no extra directory enumeration.

## GUI

- [x] Preserve raw progress separately from localized job text, tied to the current job ID.
- [x] Add a slim progress bar and Estimated percentage to the shared overview/details job strip; retain counts, path, stop control and elapsed time.
- [x] Show unknown completion without a fabricated percentage when connected to an old backend.
- [x] Keep completed 100 percent and total duration visible on the matching source/details result; partial/failed retries must not borrow an old result's 100 percent.
- [x] Test English/Chinese rendering, narrow layout, legacy data and terminal-state handling.

- [x] Reconnect to an existing backend scan on accepted catalog refresh, preserving navigation/collection and reusing the active-job timer.

## Verification and delivery

- [x] Run workspace tests, Rust formatting/Clippy, Python tests and an isolated service/MCP progress smoke test.
- [x] Compare the owned diagnostic fixture against the pre-change release runner for result equality and telemetry overhead.
- [ ] Native reconnect replay: package/signature, source-card estimate and completed 100% are verified; final running-window replay awaits manual desktop unlock.
- [ ] Independently review, commit and publish a new PR against main; record current-head native CI results.
