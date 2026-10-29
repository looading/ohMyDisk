# Collector Interaction Implementation Plan

> The user has explicitly requested the two behaviors. Execute with isolated
> file ownership in this existing clean validation worktree; no repeated design
> or execution-choice confirmation is needed for these routine details.

**Goal:** Make chart/list drag-to-collector reliable and resize the collector
with a pointer separator while preserving collection/plan safety.

**Architecture:** A UI-only shared interaction controller joins source widgets
and a single workspace owner. The App handles scoped dropped entries and panel
preference, reusing one collection validator. Views render the available height.

**Stack:** Existing Rust/iced 0.14 advanced widgets, canvas and Row::wrap; no new
dependency or platform FFI. The scanner and service remain outside this task.

## Shared interface — interaction module owner

```rust
pub struct Scope { pub scan_id: String, pub epoch: u64 }
pub struct Entry { pub scope: Scope, pub node: ohmydisk_core::Node }
pub enum Event { Dropped(Entry), HeightChanged(f32) }
pub struct Controller; // Clone + Default, UI-only, no App/widget references
pub struct PanelState; // Clone/Copy or Clone, Default
impl Controller {
    pub fn arm(&self, entry: Entry, point: iced::Point);
    pub fn is_dragging(&self) -> bool;
    pub fn cancel(&self);
}
impl PanelState {
    pub fn is_expanded(&self) -> bool;
    pub fn toggle(&mut self);
    pub fn open(&mut self);
    pub fn resize(&mut self, height: f32);
}
// Module also provides workspace(content, collector, controller, panel,
// current_scope, item_enabled, resize_enabled, on_event) -> Element
// and source(content, controller, Option<Entry>, on_click) -> Element.
```

The module owner must publish exact builder signatures before view integration.
Entry/Scope/Event are Clone. Scope compares scan and epoch. Event payloads remain
Send-compatible; Controller is never captured by a Task.

## Task 1: Interaction owner — platform/widget agent

Create only `crates/ohmydisk/src/gui/collector_interaction.rs` (focused child
modules permitted). Do not edit mod/views/sunburst/Cargo.

- [ ] Add geometry/session tests for release without hover, stale hover outside,
  first-move threshold, exact-once finish, cancellation and resize exclusion.
- [ ] Implement scoped controller, source click/drag discrimination and one
  workspace release owner. Keep RefCell borrows out of child callouts.
- [ ] Implement two-child workspace layout and separator with 64px collapsed,
  180px default expanded, 8px separator and 240px content reserve.
- [ ] Add panel preference/clamping/collapse restoration tests and render ghost,
  target highlight and separator cursor without idle frame polling.
- [ ] Report interface and freeze before Root's integrated cargo run.

## Task 2: Views — GUI agent

Own `gui/views.rs` only. Root adds App fields/messages below.

- [ ] Separate content and collector and compose the new workspace under the
  existing header/modal wrapper. Pass actual bounds via the module's layout.
- [ ] Replace row navigation Button with the source wrapper, keeping plus as a
  sibling and retaining click/navigation and hover preview behavior.
- [ ] Render collector by actual allocated height: fixed summary/actions and
  wrapped cards in vertical scroll. Resizing must reveal more rows, not blanks.
- [ ] Add compact/short collector view tests and keep scope/plan guards intact.

## Task 3: App and chart integration — Root

Own `gui/mod.rs` and `gui/sunburst.rs`.

```rust
// App additions:
collector_interaction: collector_interaction::Controller,
collector_panel: collector_interaction::PanelState,
// Message::CollectorInteraction(collector_interaction::Event)
```

- [ ] Route panel toggles/resize and successful scoped drops. Refactor collect
  into one node validator reused by plus and captured Entry; reject stale epochs.
- [ ] Add controller cancellation at scope/navigation/modal transitions and
  preserve drafts, immutable plans and cleanup authorization.
- [ ] Arm controller from real chart nodes; stop duplicate canvas drop handling,
  preserve synthetic/center click behavior and cached geometry, remove old ghost.
- [ ] Test parent/child rules, stale scope and complete unmeasured nodes.

## Task 4: Review, checks and native evidence — Root/team

- [ ] Run formatting, workspace Rust tests, Clippy, Python checks and release MCP.
- [ ] Review event capture/order, bounds, controller lifetime and plan invariants.
- [ ] Publish owned GUI commits to the existing PR, preserving concurrent scan
  work. Run native Windows/macOS CI and verify packages.
- [ ] Native fixture checks: chart/list drag in/out, cancel, divider growth/shrink,
  collapse restoration and 500x480. Record tool limitations without false passes.
