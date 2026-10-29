# Header alignment and scan timing

**Goal:** Center the native Mac traffic lights in the 48-point application
header, use the title `ohMyDisk 空间，清晰可见`, and show live/final scan time.

**Architecture:** Align the native titlebar container on AppKit's main thread,
preserving the original buttons' frames and actions. Keep its top anchored and
reapply from native lifecycle callbacks with a per-view reentrancy guard.
Keep the existing conditional 650ms job subscription, interpolate live service
time with a GUI Instant, and persist terminal service measurements with the Job.
The saved scan summaries expose optional elapsed/start/finish fields; old records
remain readable and show unknown total instead of inferred snapshot time.

- [x] Native-controls agent: inspect AppKit geometry and align the native
  standard-button group through its container without changing button frames,
  actions or hierarchy. User confirmed the container version visually aligned.
- [x] Timing agent: add optional Job timing, active monotonic reads, terminal
  persistence and summary fields; test no-progress advancement, all terminal
  outcomes and saved history through service restart.
- [x] Root: remove the decorative header circle/separator, append existing
  tagline and use the same title in the native window.
- [x] Root: render live timing separately from the progress text; expose total
  in scan heading and saved records, including sub-second scans and legacy data.
- [x] Root: run formatting, workspace tests, Clippy, release/MCP and packaging.
- [x] Team: inspect an isolated native Mac instance and run all native CI jobs.
- [x] Root: publish owned changes and acceptance evidence to the existing PR.

The existing shared checkout has concurrent, uncommitted scan-reliability work
in engine/store/tests. Do not reset, stage or overwrite it. Timing changes were
read-only three-way checked against that work and avoid its scan pipeline body.
Do not fast-forward the shared checkout over dirty paths without preserving its
changes. All verification starts from this isolated worktree's own code.

Local verification passed 181 Rust tests, 15 Python tests, formatting, Clippy
(with lint warnings), release build, isolated release MCP smoke and native Mac
packaging/signature verification. The final local executable SHA256 is
`3f440fe851ba5509ba7c8d43bfa877d587161ff992e52dbfc019e450012d6d8e`.

The earlier button-frame-only candidate was rejected by the user as still too
high. Final acceptance therefore uses the native titlebar container change
(32 to 40 points on this Mac), with the original 14-point buttons at y9. The
user confirmed this version was aligned. System sharing UI obscures automated
pixel captures; that is not presented as an independent screenshot pass.

Final source `584440d` passed Windows x64, Mac ARM and Mac Intel in
[native CI run 40921660418](https://github.com/looading/ohMyDisk/actions/runs/40921660418).
See the [acceptance record](../../verification/2026-10-30-header-scan-timing.md)
for per-platform test counts, native/manual evidence and the corrected picker
exercise that was excluded from acceptance.
