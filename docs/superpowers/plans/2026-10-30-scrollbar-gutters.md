# Scrollbar Content Separation

**Goal:** Keep scrollbars clear of text, cards and actions throughout OhMyDisk.

**Design:** All six production scroll regions use iced 0.14.2's embedded scrollbar layout. Keep the existing 10-point track and thumb, and add 8 points between the track and content. iced reserves the resulting 18-point gutter only when that axis overflows. Vertical regions reserve space on the right; the breadcrumb region reserves space below. Preserve the existing scrolling and collector drag behavior.

**Scope:** Disk/history overview, directory preview, collector cards, cleanup review, cleanup results, and horizontal breadcrumbs. No service, cleanup policy, dependency or snapshot changes.

- [x] Add shared `gui/scroll_area.rs` helpers for vertical and horizontal scrolling, with actual iced layout assertions for overflow and no overflow at narrow and regular sizes.
- [x] Route all six production regions through the helpers. Update the existing scrolled collector drag regression to exercise the same configuration.
- [x] Run formatting, relevant Rust widget/layout tests, Clippy, and release build/package verification. Review the final diff independently.
- [x] Open the rebuilt application and verify startup/display. Overflow and narrow layouts are covered by actual iced geometry tests. Native resize automation returned `noWindowsAvailable`; native overflow appearance remains unverified. No scan, cleanup plan or deletion was performed for this GUI check.
- [ ] Commit on `codex/scrollbar-gutters` and open a focused PR against `main`; PR #3 has already merged as `5c42205`, with an identical source tree to the starting `755567f`. Do not alter the other chat's dirty backend worktree.
