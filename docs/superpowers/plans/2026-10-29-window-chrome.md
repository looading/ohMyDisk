# Integrated Window Chrome Implementation Plan

> For agentic workers: the user has approved the concrete design and has already
> requested multi-agent execution. Implement in the existing managed worktree;
> do not ask for another design or execution-choice approval.

**Goal:** Integrate the native window frame with the application header on macOS
and Windows, with functional window gestures and a usable narrow layout.

**Architecture:** A focused chrome module translates platform window events and
caption gestures into iced tasks. The App owns that state and maps messages.
Adaptive views and overlays consume the current logical window size.

**Tech Stack:** Existing Rust 2024, iced 0.14.0, iced_winit 0.14.1 and winit
0.30.13; the Mac drag correction uses already-locked objc2/AppKit wrappers
for a local mouse-down view, without replacing the runner.

## Global constraints

- Preserve Mac native decorations and traffic lights; configure title fusion at
  creation. Windows gets explicit client caption and resize regions.
- Minimum 500x480 logical pixels; compact presentation below 1000 pixels wide.
- One press emits one caption action. Controls must not initiate a drag.
- No continuous frame/event polling; no claim of Windows 11 hover Snap support.
- Closing GUI must not send cancellation or execute cleanup. Snapshot bytes,
  draft scan ownership, immutable plans and independent authorization persist.

## Task 1: Window chrome — platform agent

Create `crates/ohmydisk/src/gui/window_chrome.rs` only. Root integrates it.

Interfaces:

```rust
pub(super) fn settings() -> iced::window::Settings;
pub(super) struct State { pub size: iced::Size, /* private window identity/status */ }
pub(super) enum Action { Drag, ToggleMaximize, Minimize, Close, Resize(iced::window::Direction) }
pub(super) enum Message { /* Action, window events, targeted state replies */ }
impl State {
    pub fn initial_task() -> iced::Task<Message>;
    pub fn subscription() -> iced::Subscription<Message>;
    pub fn update(&mut self, message: Message) -> iced::Task<Message>;
    pub fn controls(&self) -> iced::Element<'_, Message>;
    pub fn is_maximized(&self) -> bool;
}
pub(super) fn drag_area<'a, M: Clone + 'a>(
    content: iced::Element<'a, M>, on_action: impl Fn(Action) -> M + 'a,
) -> iced::Element<'a, M>;
pub(super) fn frame<'a, M: Clone + 'a>(
    content: iced::Element<'a, M>, maximized: bool,
    on_action: impl Fn(Action) -> M + 'a,
) -> iced::Element<'a, M>;
```

- [x] Add gesture regression tests: single press is Drag, second nearby press is
  only ToggleMaximize, a moved window resets the double-click candidate.
- [x] Implement settings, event-driven State and controls with iced `drag`,
  `drag_resize`, `minimize`, `toggle_maximize`, `close` and state queries.
- [x] Add Windows edge/corner hit testing and matching cursors; test all eight
  directions, the interior and maximized suppression.
- [x] Verify stale window replies cannot update a different/closed window.
- [x] Run module tests and Windows target check; report exact interface changes.

## Task 2: App and adaptive views — root

Modify `gui/mod.rs` and `gui/views.rs`; retain `space_view` and sunburst metrics.

```rust
// New App state and messages:
chrome: window_chrome::State,
compact_preview: bool,
// Message::WindowChrome(window_chrome::Message), Message::ToggleCompactPreview
// Initialization/settings and subscriptions are delegated to window_chrome.
// Compact = chrome.size.width < 1000.0.
```

- [x] Map startup/window tasks and subscription, then route actions independently
  of application navigation and server jobs.
- [x] Build a header with reserved native Mac button space and Windows controls;
  keep PickFolder and caption actions out of the drag area.
- [x] Switch compact analysis between chart and the existing preview pane; make
  sources, navigation and collector fit 500 logical pixels.
- [x] Bound review/result panels to the viewport and scroll content without
  hiding the confirmation phrase, back action or execution controls.
- [x] Add layout-boundary and compact draft/plan-preservation regression tests.
- [x] Run formatting and targeted GUI tests after integration.

## Task 3: Integration, review and distribution — root/team

- [x] Run `cargo fmt --all --check`, `cargo test --locked --workspace`,
  `cargo clippy --locked --workspace --all-targets`, and Python tests.
- [x] Build release and run fixture-only MCP smoke; compare scanner source to
  baseline to confirm this window change does not alter measurement behavior.
- [x] Review event routing, modal hit behavior, native platform configuration and
  sizing constraints independently.
- [ ] Update the existing PR description for the final window feature scope;
  run native Windows x64, macOS ARM and Intel CI and verify artifacts.
- [ ] Finish isolated unlocked-Mac gesture and confirmation/back acceptance.
  Record unavailable native interaction coverage as such, not as a pass.

The approved design is
[`2026-10-29-window-chrome-design.md`](../specs/2026-10-29-window-chrome-design.md).

## Verification record

The window code passed 145 local Rust tests, 11 Python tests, formatting and
Clippy (with lint warnings). Release MCP smoke passed both supervised and auto
modes. A standalone iced-only harness type-checked the Windows window module;
whole-application local MSVC cross-check requires the Windows C SDK for bundled
SQLite, so native CI is the full-build gate. Independent reviews found no
blocking window/widget or modal authorization defects.

A separate user-authorized scan-reliability task is editing core/service files
in the shared checkout. This task commits only the GUI, its README changes and
this plan; clean-checkout verification keeps those backend edits separate.

## Mac drag correction

Native acceptance recorded all non-drag checks, but fixed-window coordinates
stayed unchanged after the iced-task drag. `window_chrome/macos_drag.rs` therefore
installs a local transparent MainThreadOnly NSView that uses the original mouse
event. It shares header/picker geometry constants with views.rs, avoids controls
and content, uses parent ownership, and has no global swizzle or periodic task.
Unsafe boundaries document ABI, main-thread access and retained lifetimes.
Bounds/flipped/invalid-viewport tests supplement the existing gesture tests.
The native correction must pass actual before/after window bounds, followed by
full tests, lint, MCP and native CI.
