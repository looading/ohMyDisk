# Application icon integration plan

**Goal:** Use the approved icon in native application windows and macOS/Windows
distribution artifacts.

**Architecture:** Derive checked-in platform assets from the unchanged approved
PNG. Embed a small RGBA window icon in the Rust executable, compile Windows icon
resources at build time, and declare/copy the macOS bundle icon before signing.
Packaging verifies the actual embedded resources before uploading an archive.

**Stack:** Rust/iced 0.14, winresource, existing Python packaging, offline Pillow
asset conversion. Rust minimum remains 1.88. No runtime image codec is added.

The user explicitly approved the design and requested integration. Continue in
the existing clean validation worktree with separate file ownership; preserve
the concurrent scanner work in the shared checkout.

## Tasks

- [x] Root: export `assets/icons/omd.icns`, `omd.ico`, and `omd-window-64.rgba`
  from `omd-app-icon-v1.png`; retain a reproducible conversion script and source
  hashes. Verify transparency, ICO sizes 16/24/32/48/64/128/256, and ICNS contents.
- [x] Root: set `window_chrome::settings().icon` from embedded 64×64 RGBA bytes.
  Use iced's existing `window::icon::from_rgba`, without filesystem IO or PNG
  decoding at startup. Check actual icon dimensions and embedded bytes.
- [x] Windows agent: add `crates/ohmydisk/build.rs` and a build dependency.
  Check target OS, compile `omd.ico` as resource ID 1, preserve CLI/MCP subsystem,
  inherit workspace version, and propagate resource compiler failures.
- [x] Packaging agent: declare `CFBundleIconFile`, copy ICNS before codesign,
  validate PE icon-group/image payloads against the approved ICO, and gate the
  actual release packaging in native CI. Cover missing and mismatched resources
  in the existing packaging suite.
- [x] Root/team: run `cargo fmt --all --check`,
  `cargo test --workspace --locked`,
  `cargo clippy --workspace --all-targets --locked`, and
  `python3 -m unittest discover -s tests -p 'test_*.py' -v`.
- [x] Root/team: build/package the native Mac app, verify plist/icon/signature,
  and run native Windows/MSVC, Mac ARM and Mac Intel CI. Validate the local Mac
  ZIP and record the independent CI-archive download boundary separately.
- [x] Root: publish only owned icon/build/package changes to the existing PR,
  preserving the concurrent backend diff and keeping the PR draft for that work.

## Local verification

174 Rust tests and 15 Python tests passed. Formatting, Clippy (with lint warnings),
release build, isolated release MCP smoke and native Mac packaging/signature
verification passed. NSWorkspace rendered the new icon from the signed app.

All derivative hashes match the asset manifest. The eight ICNS PNG blocks match
the approved source pixels exactly. The 16/32-pixel native ARGB blocks preserve
alpha and opaque colors exactly; their semitransparent RGB quantization is at
most one channel level and their premultiplied pixels match exactly. Verification
reads those ARGB blocks directly: `iconutil --convert iconset` is lossy for them,
as also documented by the [icon tool maintainers](https://github.com/electron-userland/electron-builder-binaries/blob/master/packages/icons/CHANGELOG.md#123).

All three platform jobs passed for `a00f998` in
[run 40907202426](https://github.com/looading/ohMyDisk/actions/runs/40907202426).
The Windows package step checked the actual RC/linker output. The local Mac ZIP
also passed checksum, archive integrity, bundle declaration and exact ICNS-byte
checks. Independent re-download of CI ZIPs was bandwidth-limited; do not count
those post-download checks as completed. Native CI's own checks and uploaded
artifacts remain available through the workflow run.
