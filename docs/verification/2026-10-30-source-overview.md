# Unified disk and folder overview

The overview now shows one list of native disks and chosen folders. There is no separate Saved scans section. Each source exposes its latest attempt and usable result, with View results and Scan/Rescan actions. Custom folder entries also expose Remove; it updates preferences without deleting source files or releasing backend snapshots.

Picked folders are canonicalized off the GUI thread and registered before scanning. Repeated paths reuse the same entry. The catalog uses lossless native path comparison keys while preserving the original path spelling for I/O, including Windows verbatim paths. Removal tombstones prevent retained scan records from restoring a removed entry; explicitly adding that folder again clears its tombstone.

Scans launched from the overview finish there. A successful details rescan replaces the displayed snapshot only while the same root remains open. Failed retries retain access to earlier usable results. Source refresh gathers native sources and all scan-summary pages before reconciliation, rejects stale refresh epochs, and retries invalidated pagination once. Snapshot storage and MCP APIs remain unchanged.

Language and folder settings share the existing serialized, atomic preference writer and native shutdown flush. Oversized configuration writes are rejected before replacing the last readable document. Folder-card usage comes from measured file attribution rather than the capacity used by the containing volume.

## Verification

- Rust workspace tests passed: 226 total, including 118 application tests on macOS. Coverage includes path deduplication, non-lossy path identity, latest-attempt/result fallback, removal/re-addition and restart, mixed preference updates, pagination invalidation, modal/active-scan guards, and overview/detail scan completion behavior. Windows additionally exercises verbatim drive-prefix comparison without changing the I/O path.
- Formatting and Clippy completed; project lint warnings remain. Python packaging/inventory tests: 15 passed.
- Release build, macOS packaging and strict ad-hoc signature verification passed.
- Independent reviews found no blockers in catalog identity, source-card behavior, asynchronous flow, persistence, or removal safety.
- Native startup used an isolated profile with two completed scans of one owned fixture. The overview rendered one custom-folder card with View results, Rescan and Remove beside the two native volume cards, and no Saved scans section. The preferences document contained one registered root while both scan records remained available. The fixture marker SHA-256 was unchanged; the profile contained no cleanup plans or executions.

Native coordinate actions returned `noWindowsAvailable`, so native clicks for View/Rescan/Remove were not completed at this checkpoint. Their message/state behavior is covered by application tests. The desktop availability question remains pending; no click-success claim is made. No real user directory was scanned or cleaned during this verification.
