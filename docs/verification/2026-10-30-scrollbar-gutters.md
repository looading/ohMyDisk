# Scrollbar gutters — 2026-10-30

All six production scroll regions now use shared embedded scrollbar helpers: sources/history, directory preview, collector cards, cleanup review, cleanup results, and horizontal breadcrumbs. The existing 10-point track and thumb are retained. Overflow reserves an additional 8-point gap, for an 18-point gutter; fitting content uses the full area.

## Verification

- `cargo fmt --all --check`: passed.
- `cargo test --locked -p ohmydisk`: 75 passed, including the existing actual-widget scrolled collector drag test using the shared helper.
- Two new actual iced layout tests cover vertical wrapped content and horizontal content, each at 500- and 1100-point widths, with overflow → fit → overflow transitions using the same widget tree. Content bounds exclude exactly 18 points while overflowing, and cards stay inside those bounds.
- Regression check: temporarily removing embedded spacing causes both layout tests to fail (vertical content width 500 instead of 482; horizontal height 80 instead of 62). Restoring the final helper restores all 75 passing application tests.
- `cargo clippy --locked -p ohmydisk --all-targets`: passed with existing project warnings; the new helper/test module emits none.
- Python packaging/inventory tests: 15 passed.
- Release build for macOS with deployment target 13.0, application packaging, and strict ad-hoc signature verification: passed.
- Independent source review found no blocker and checked all six call sites, breadcrumb shrink sizing, wrapped cards, and iced's layout/drawing/pointer translation behavior.

## Native observation and limits

The rebuilt macOS application was launched and raised successfully. Its sources/history overview displayed correctly with fitting content. Native resize automation returned `noWindowsAvailable`, so native overflow/resize appearance is not claimed as verified; the actual iced layout tests above verify that geometry. No new scan, cleanup plan, or deletion was performed during this GUI check. Windows desktop interaction was not exercised locally; the same helpers and layout tests run in the native CI matrix.
