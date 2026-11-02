# English and Chinese UI

**Goal:** Default to English; switch between English and Simplified Chinese immediately and remember the user's selection. Provide concise, equivalent English and Chinese READMEs.

**Design:** A small typed language module chooses paired English/Chinese copy. Stateful banners and progress retain both variants so existing messages switch without another backend event. Preferences live in `<data-dir>/preferences.json`, independent of scan storage; atomic replacement runs off the UI thread, with one serialized writer that owns the latest selection independently of GUI completion messages and drains pending writes on normal close. Missing preferences default to English. Invalid preferences fall back to English with a visible warning. Failed saves leave the live selection usable and report the error.

**Scope:** GUI labels, title/tagline, status/phase/duration text, cleanup review/results and Windows caption tooltips. Filename/path strings, technical OS/service diagnostics, MCP names, API fields and scan data remain unchanged. Language switching preserves navigation, scans, collection and review; it cancels an active drag and clears the destructive confirmation text so a token from the previous language cannot authorize an action.

## Tasks

- [x] Add language selection, deferred bilingual text, bounded preferences reading and atomic private preference writes. Verify first-run English, round trips, malformed input, and write failure.
- [x] Localize app state and all views, including elapsed time and native caption tooltips. Add a compact top-bar language dropdown that remains reachable at the minimum window size.
- [x] Verify live switching preserves app state, current messages and cleanup plan identities; only the current locale's destructive confirmation token is accepted. Verify rapid selection changes serialize writes and persist the latest selection.
- [x] Replace README with concise English instructions, add matching `README.zh-CN.md`, cross-link both, and include both in platform ZIPs with packaging verification.
- [x] Run formatting, app/workspace tests, Clippy, release packaging and a native runtime switch/reopen check. Review independently. Native macOS startup, runtime switching, stored preference and restart restoration are verified; Windows desktop interaction is not claimed.

**Implementation ownership:** Root owns language/preferences/duration/native captions. Separate agents own app state, views, and README/packaging; shared interfaces are agreed before editing. No changes to the other chat's backend worktree.

**Integration target:** Publish a focused PR on top of `codex/scrollbar-gutters` (PR #4), which remains open.
