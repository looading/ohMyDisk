# Window chrome acceptance — 2026-10-29

The user approved the integrated-titlebar design. Product code is in `ba76604`
and its native Mac drag correction `f1cd809`. No scan, storage or MCP behavior
was changed by these window commits. A separate scan-reliability task continues
on the shared 0.2.0 branch.

## Automated and native CI checks

- 148 local Rust tests passed, including gesture/state and native-band geometry
  regressions. Formatting, Clippy (with warnings) and 11 Python tests passed.
- Release MCP passed auto and supervised fixture workflows; the correction was
  rechecked in auto mode. GUI startup is not involved in MCP/service commands.
- [Native CI for f1cd809](https://github.com/looading/ohMyDisk/actions/runs/40846241988)
  passed Windows x64, macOS Apple Silicon and macOS Intel. Packages were checked
  for SHA-256, archive integrity, architecture, version and company markers.

## Local-release Mac observations

Native interaction used an isolated temporary app identity, state and real-write
fixtures. The same-code copy's text sections were checked; it is accurately
recorded as a local release build, not the CI distribution binary.

- Native close/fullscreen/minimize controls remained exposed by accessibility;
  the title text was hidden and the header blended with the content.
- Header double-click zoomed and restored the window.
- Native resize reached 1000x960 physical pixels at Retina scale 2, or 500x480
  logical pixels. Navigation, context switches and collector stayed reachable.
- Compact chart/list switching retained the collection.
- Permanent review allowed neutral input, clearing and returning; its execute
  button remained disabled. A new trash review also opened and returned.
  Read-only audit showed zero executions and all six fixture files still present.
- In individual approximately ten-second samples at 500x480, unfocused permanent
  review added 0.00 CPU seconds and focused neutral-text caret added 0.13 seconds
  (1.30% of one core). These are coarse ps TIME observations, not FPS or a general
  performance guarantee. They must not be compared as an improvement ratio with
  earlier, larger-window observations.

![500x480 local native layout](images/window-chrome-500x480.jpg)

## Drag evidence and limits

The first automated drag's accepted input did not establish displacement. A
native original-event handler was added to avoid depending on currentEvent in
a deferred task. Diagnostic builds confirmed installation and mouseDown receipt.
An unmodified system-titlebar control also failed to show automated displacement,
so the automated result cannot establish an application-specific cause.

The user subsequently reported that the designated test window **did move with
the real mouse**. Mac drag is recorded as manually confirmed. No quantitative
manual delta or complete in-process trajectory was captured. Diagnostic patches
and logs remain outside the repository; product logging stays quiet by default.

Windows native gestures were not manually exercised. Windows 11 maximize-button
hover Snap Layout is not implemented; see the approved design for that boundary.
