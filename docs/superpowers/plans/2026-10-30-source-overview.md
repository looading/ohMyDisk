# Unified disk and folder overview

**Goal:** Replace the Saved scans section with one persistent list of scan locations. Each disk or folder exposes its latest attempt, usable result and rescan action. Custom folders can be removed from the list without deleting source files.

**Design:** Keep immutable backend snapshots and cleanup-plan references. A GUI catalog stores registered custom roots and removal tombstones in the existing atomic preferences document. Repeated canonical folder selections reuse the same entry; lossless path keys avoid display-string collisions and preserve case-sensitive filesystem semantics. Native disk cards are discovered automatically and do not get a remove action.

The latest attempt and latest viewable result are selected separately by scan start time. A failed retry can therefore retain access to previous results while exposing its failure. Source discovery and all scan-summary pages are loaded as one epoch-bound refresh before catalog reconciliation. Removed roots stay hidden across retained scan history until explicitly added again. Folder usage is measured file attribution, not the used capacity of its containing disk.

- [x] Add catalog matching, deduplication, latest-result selection and removal/re-addition tests.
- [x] Extend the shared preference writer to persist language and folder changes coherently, retaining shutdown protection and the bounded atomic configuration format.
- [x] Register folder selections off the UI thread, link results to source cards, remove the old history UI and keep Remove distinct from filesystem cleanup.
- [x] Verify paginated refreshes, stale responses, active-scan removal guards, bilingual layouts, failed-retry fallback, persistence and file preservation.
- [ ] Run Rust/Python checks, release packaging and isolated native overview interaction checks; review independently and update the GitHub change against the current main branch state.

Native startup/dedup rendered successfully. Coordinate interactions are pending desktop availability after `noWindowsAvailable`; no native-click success is claimed.
