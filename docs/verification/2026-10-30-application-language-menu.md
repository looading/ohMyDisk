# macOS application-menu language selection — 2026-10-30

The existing macOS application menu now contains `Language` / `语言` → `English` / `中文`, with check states derived from the accepted GUI language. Existing About, Services, Hide, Show All and Quit items retain their native actions and shortcuts. The top-bar selector remains available on all platforms; Windows window chrome and client layout are unchanged.

Native items are inserted and synchronized only through iced's main-thread `window::run` callback. Each item strongly owns its action target through `representedObject`; the weak target points to that same object. Targets retain only typed Rust channel state and their fixed language. No native handles are stored globally or sent across threads.

Native callbacks publish revision-tagged language intents. The GUI rejects commands older than a newer top-bar or native intent, then uses the same language application/persistence path. A stable asynchronous watch subscription wakes on activation; no polling timer is added. Queued checkmark updates read current state when they actually run.

## Verification

- Rust workspace tests: 205 passed, including 97 application tests. Coverage includes native-event delivery without idle polling, stable subscription identity, stale commands both before stream consumption and after queueing, menu-origin switching that preserves scan/navigation/collection/review/job/result state, and normal close before the native command reaches the GUI.
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

A native intent also remains pending until GUI acceptance. Normal close feeds the latest pending intent into the existing preference writer before flushing; a newer top-bar choice clears it. Two shutdown regressions verify both paths. Removing the close-time intent drain makes the undelivered-native-choice test fail because no preference file is saved; restoring it makes the complete workspace suite pass.

## Native Quit persistence regression

Standard macOS Quit uses AppKit termination and can bypass Rust stack destruction. A main-thread notification observer now invokes the same shutdown capsule synchronously for `NSApplicationWillTerminateNotification`. Its owner is the parent Language menu item; repeated synchronization removes the prior registration before replacing the observer. The existing application delegate and Quit action remain unchanged. The callback contains unexpected Rust panics and reports save failure without unwinding into Objective-C.

A real native comparison verified the exit path in an isolated empty profile. After GUI startup, an empty directory named `preferences.json` temporarily caused the asynchronous language save to fail visibly. The directory was then removed, restoring a valid destination, and the preserved native Quit command was invoked. The baseline without the termination observer left no preference file; the candidate saved `zh-CN`. This verifies the AppKit exit callback rather than relying only on a synthetic `drop(app)` test. The profile retained zero scans, plans and executions. Its GUI and owned backend were closed afterward.

An additional unit test invokes the actual shutdown capsule while App remains alive and the native command has not reached the GUI, proving that it persists the pending choice without a destructor or another event-loop turn.
