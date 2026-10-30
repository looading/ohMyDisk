# Application-menu language selection

**Goal:** Add language selection to the existing macOS application menu and keep it synchronized with the top-bar selector and persisted preferences.

**Design:** Insert `Language` / `语言` with fixed `English` and `中文` choices into the existing AppKit app menu. Check exactly the active language. Preserve the menu's standard About/Services/Hide/Quit items and shortcuts. All native objects remain on the AppKit main thread, created or updated through iced `window::run`. Menu items retain their action target through `representedObject`; that target owns only a typed asynchronous sender. No native handles are global or sent to worker threads.

Native selections wake an asynchronous subscription and enter the existing `SetLanguage` handler. A bridge stores the current GUI language independently of queued native callbacks so delayed callbacks cannot restore stale checkmarks. Revision-tagged native intents are also rejected if a newer native or top-bar choice has superseded them, both before and after event-stream delivery. Top-bar selection and saved startup preferences synchronize the same menu. A native intent remains pending until GUI acceptance, and normal close drains that intent into the preference writer even if no GUI command was delivered. AppKit native termination also invokes the same shutdown capsule from an application-will-terminate notification observer, because native Quit may bypass Rust destructors. A newer top-bar choice invalidates it. There is no polling timer. Windows retains its existing top-bar language menu and window layout.

- [x] Add the event bridge and native insertion/update adapter, including lifecycle and action-target ownership checks.
- [x] Wire startup, focus and language changes through the existing GUI state handler; check progress/collection/review preservation and stable subscription identity.
- [x] Document the native menu entry in both READMEs.
- [x] Run formatting, Rust/Python checks, release packaging and native menu selection/synchronization/restart validation. Review the bridge and FFI independently.

Extend the existing bilingual UI PR #5 rather than creating another dependent PR. Do not modify the other chat's backend worktree.
