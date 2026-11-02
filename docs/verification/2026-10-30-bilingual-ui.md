# English and Chinese — 2026-10-30

## Behavior

- A profile without a language preference starts in English. The top-bar `English` / `中文` selector updates application copy and the window title immediately.
- Preferences are stored in `<data-dir>/preferences.json` as `en` or `zh-CN`. Reads are bounded to 64 KiB. Invalid input falls back to English with a warning. Writes preserve unknown settings and atomically replace the document.
- A shared writer owns the latest selection before any async task runs. Disk writes are serialized and coalesced without holding the selection lock during I/O. Normal window shutdown drains unsaved state independently of GUI completion messages.
- Language switching preserves scans, navigation, jobs, collection, plans and results. Stateful banners/progress retain both translations. Switching cancels an active drag and clears permanent-delete confirmation input; the current locale requires `DELETE` or `永久删除` at both the button and execution handler.
- Paths, filenames, technical OS/service diagnostics and MCP protocol names/values remain unchanged. No backend schema changes.
- Both README files are concise, cross-linked and included in macOS/Windows packages. Current development artifacts and tagged releases are distinguished; no links assume that the older latest release contains these new features.

## Automated verification

- `cargo fmt --all --check`: passed.
- `cargo test --locked --workspace`: 198 passed (90 application tests plus 108 core/platform/service tests).
- `cargo clippy --locked --workspace --all-targets`: completed with lint warnings; the new locale/preferences modules emit none.
- Python packaging/inventory suite: 15 passed, including exact English/Chinese README payload checks for both platforms.
- macOS release build with deployment target 13.0, application packaging and strict ad-hoc signature verification: passed. Both README payloads and ZIP CRC were also checked in the actual archive.
- Persistence regressions cover malformed configuration, replacement/reopen, write errors, a blocked earlier write followed by a new selection and shutdown drain, and application shutdown without delivering any GUI save-completion messages.
- Independent review found a pending-save shutdown gap, then re-reviewed and accepted the shared writer/drop drain fix.

## Native macOS observation

Used an isolated empty profile and a uniquely identified test bundle containing the final release executable. A separate headless service avoided ambiguous application identities during UI automation.

1. With no preferences file, the GUI and native window title appeared in English.
2. Clicking the language dropdown exposed both choices. Selecting Chinese updated the title, source cards, overview, actions and collection labels immediately; the stored code was `zh-CN`.
3. Exited through the native application menu and reopened the same profile: Chinese was restored.
4. Switched back to English: the interface/title updated immediately and the stored code became `en`.
5. The isolated profile retained zero scans, zero cleanup plans and zero executions. The test GUI and owned headless service were closed after verification. The regular rebuilt application was opened and raised in English afterward.

The first native pass exposed an input bug: the existing AppKit drag band still included the new language selector. Its shared trailing exclusion now covers the 86-point selector, both 8-point gaps and 120-point folder action (222 points). The native geometry regression failed with the old 128-point exclusion and passed after the fix; the actual dropdown then opened and switched successfully. Geometry was checked at the minimum 500-point width and larger sizes. A separate header review confirmed both platform layouts.

Windows desktop interaction and language switching during a real native scan were not exercised locally. State preservation, ongoing progress translation and destructive confirmation were checked with application tests; the native CI matrix provides Windows compilation and test coverage.
