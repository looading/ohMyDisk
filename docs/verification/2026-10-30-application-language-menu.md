# macOS application-menu language selection — 2026-10-30

The existing macOS application menu now contains `Language` / `语言` → `English` / `中文`, with check states derived from the accepted GUI language. Existing About, Services, Hide, Show All and Quit items retain their native actions and shortcuts. The top-bar selector remains available on all platforms; Windows window chrome and client layout are unchanged.

Native items are inserted and synchronized only through iced's main-thread `window::run` callback. Each item strongly owns its action target through `representedObject`; the weak target points to that same object. Targets retain only typed Rust channel state and their fixed language. No native handles are stored globally or sent across threads.

Native callbacks publish revision-tagged language intents. The GUI rejects commands older than a newer top-bar or native intent, then uses the same language application/persistence path. A stable asynchronous watch subscription wakes on activation; no polling timer is added. Queued checkmark updates read current state when they actually run.

## Verification

- Rust workspace tests: 202 passed, including 94 application tests. Coverage includes native-event delivery without idle polling, stable subscription identity, stale commands both before stream consumption and after queueing, and menu-origin switching that preserves scan/navigation/collection/review/job/result state.
- Formatting and Clippy completed successfully (lint warnings remain); Python packaging/inventory suite: 15 passed.
- macOS release build, packaging and strict ad-hoc signature verification passed.
- Independent review accepted the revision ordering, lock order, startup bridge identity, AppKit main-thread ownership and preservation of standard menu items.

## Native macOS check

Used a uniquely identified test app with an isolated empty state and an external headless backend:

1. The native application menu exposed one `Language` submenu containing both choices alongside the existing standard commands.
2. Activated `中文` through its native accessibility action: the application window/title and top-bar selector changed to Chinese; the parent menu became `语言`; preferences contained `zh-CN`.
3. Quit through the preserved native Quit action and reopened the profile: the GUI and application menu restored Chinese.
4. Switched to English through the top-bar selector: window/title and application-menu label returned to English; preferences contained `en`. Repeated focus/reopen/menu updates did not duplicate the language submenu.
5. The isolated state contained zero scans, zero cleanup plans and zero executions. Its GUI and owned backend were closed after the check.

CUA exposed native menu titles/actions but not checkmark attributes; exclusive check-state assignment was source-reviewed. Windows retains the existing language selector and is covered by the native CI build/test matrix rather than a local Windows desktop check.
