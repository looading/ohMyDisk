# Project drag/drop and resizable collector

The user explicitly requested two interactions: drag projects/items into the
collector, and drag the boundary between content and collector to adjust their
space. These concrete requirements authorize the implementation below. This
continues the approved DaisyDisk-style application collector; it does not add
Finder/Explorer imports or change cleanup authorization.

## Drag and drop

Real Node sectors and directory-list row bodies use one UI interaction owner.
Pressing records a candidate containing scan ID, view epoch and an immutable
Node copy. A movement of at least 6 logical pixels begins a drag. Releasing a
candidate without that movement preserves the existing click/navigation action.
The separate plus button remains available and never starts a row drag.

The workspace owner computes the collector rectangle from its actual layout.
On release it tests the current pointer against that rectangle, submits at most
one scoped drop and always clears the session. Hover only controls visual
feedback. Source widgets reset their local press state without independently
submitting a drop. This handles captured release and a first move that jumps
directly outside the chart. The ghost label is drawn over the workspace while
dragging, rather than only inside the chart. Idle frames add no empty overlay.

Escape, window exit/unfocus, navigation/scope change, compact-pane switching and
modal activation cancel the gesture. Synthetic free/system/aggregate sectors
never provide an Entry. A drop only changes the pending collection and must use
the same eligibility, scan ownership, root exclusion, completeness, parent/child
normalization and 500-target limit as the plus button. A real complete node with
unknown physical measurement remains eligible under the existing rules.

## Resizable boundary

An 8-pixel separator has a visible grip and vertical resize cursor. Item dragging
and separator dragging are mutually exclusive. Height is calculated from actual
workspace bounds, not the full window height. Keep the collapsed bar reachable
and reserve at least 240 logical pixels for content whenever the viewport allows.

Default expanded height is 180 logical pixels; the collapsed bar is 64. User
divider movement changes a session preference. Window shrink clamps only the
displayed height; growing the window restores the preference. Collapse retains
the previous useful expanded height. The header/actions stay reachable; a short
collector displays its summary, while a taller collector uses wrapped cards in a
vertical scroller so increased height exposes additional items.

## Architecture

`gui/collector_interaction.rs` owns the same-thread controller, row drag source,
workspace layout, drop hit testing, ghost/highlight and separator interaction.
Its shared state is Rc<RefCell> confined to the iced UI thread; no controller is
sent through Tokio tasks, and it stores no widget/App references or cycles.
RefCell borrows end before child callbacks or drawing. Public messages contain
owned scoped Entry data, not the controller.

`gui/mod.rs` owns collection validation and panel preference. `gui/views.rs`
composes the workspace, draggable rows and height-adaptive wrapped collector.
`gui/sunburst.rs` arms the same controller for real nodes and retains typed
synthetic navigation and cached geometry. Release coordinates, not stale hover,
are authoritative. No scan/MCP/storage changes or new dependency is required.

## Verification

Cover stale/missing hover, direct jump into target, captured release, exact-once
drop, click versus drag, Escape/unfocus/window exit, stale scan/epoch, disabled
modal, parent/child normalization and unknown-measurement eligibility. Cover
divider bounds at 500x480, long banners through actual workspace size, remembered
height after viewport shrink, collapsed/expanded transitions and item/resize
exclusion. Run workspace tests/lint and fixture-only MCP, then native CI.
Native acceptance uses owned temporary fixtures and does not execute cleanup.
