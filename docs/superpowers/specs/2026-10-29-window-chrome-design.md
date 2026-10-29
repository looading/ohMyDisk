# Integrated window chrome

The user approved the cross-platform frameless/integrated-titlebar proposal on
2026-10-29. This extends the existing GUI work in PR 3; it does not change the
scanner, MCP protocol or cleanup authorization.

## Appearance and operating-system behavior

macOS keeps native decorations, traffic lights, resize behavior and fullscreen.
Use iced's `title_hidden`, `titlebar_transparent` and `fullsize_content_view` at
window creation. The application header reserves the native button area and no
longer sits beneath a separate opaque title strip. Keep ordinary system keyboard
and window actions. Do not substitute a borderless Mac window: winit 0.30 does
not implement programmatic `drag_resize_window` on macOS.

Windows uses `decorations=false`, a header with minimize/maximize-or-restore/close
controls, and transparent edge/corner resize hit regions. Dragging and resizing
enter native OS modal loops through iced tasks. Double-clicking a drag region
toggles maximization; one press emits one action, so double-click does not also
start a conflicting drag task. Header controls and the scan-folder button are
outside the drag region. Maximize state is queried after OS resize/focus changes,
including changes made outside the app's caption buttons.

Normal drag/keyboard snapping uses winit's retained Windows window styles.
Windows 11's maximize-button hover Snap Layout menu requires `HTMAXBUTTON`
nonclient hit testing and is not promised by an ordinary iced button. This change
does not introduce a Win32 subclass or a new chrome dependency merely to claim
that behavior. Keep OS native behavior as the fallback on other platforms.

## Small windows

Minimum client size becomes 500 by 480 logical pixels. At widths below 1000,
the right preview becomes a chart/list switch rather than a permanent 350-pixel
column. Navigation condenses ancestors, labels wrap or shorten, source cards and
the collector fit the available width. The confirmation and result panels use
available viewport dimensions with scrollable content, retaining accessible
back/close/execute controls and the same explicit permanent-delete phrase.
No byte calculation or selection ownership changes with the viewport.

## Lifecycle and implementation boundary

`gui/window_chrome.rs` owns native settings, window identity/status, caption
actions, title drag gesture routing and Windows resize regions. The App maps its
messages and continues to own application data. Window-event subscriptions exclude
redraw notifications and do not add periodic polling. Closing the GUI closes its
window; already-accepted service jobs follow the existing independent lifecycle.

`gui/views.rs` owns adaptive presentation. `gui/mod.rs` integrates chrome state,
the preview switch and viewport-aware overlays. Keep the existing sunburst cache
and snapshot/plan semantics. Background CPU observed during locked-screen testing
remains an observation; this feature does not make an unverified power-use claim.

## Verification

Test title single/double-press routing, movement reset, resize edges/corners,
maximized resize suppression and stale window-state replies. Exercise layouts at
500x480, 800x600, 1000x760 and 1370x900. Existing metric/selection/authorization
tests remain valid. Run workspace tests, formatting, Clippy, native three-platform
CI and fixture-only MCP smoke. On an unlocked Mac verify the integrated native
controls, dragging, resizing, compact preview and confirmation/back flows with
isolated state. Windows native GUI gestures need actual native evidence before
claiming they were manually exercised.

## Sources

- Locked iced 0.14.0 / iced_winit 0.14.1 / winit 0.30.13 local source.
- [iced window tasks](https://docs.rs/iced/0.14.0/iced/window/index.html).
- [Microsoft Snap Layout requirements](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/ui/apply-snap-layout-menu).

Two alternatives were considered: drawing caption controls on both platforms,
which loses reliable Mac native resizing, and adding iced-window-chrome 0.1.0,
which patches after creation and still leaves resize handles to application code.
The selected approach uses the already-pinned ecosystem APIs.
