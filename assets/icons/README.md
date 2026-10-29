# ohMyDisk app icon

`omd-app-icon-v1.png` is the original design master for the ohMyDisk desktop
application, generated with the built-in image generation tool on 2026-10-29.

- 1254 × 1254 pixels, RGBA PNG with genuine transparency outside the tile.
- Deep indigo rounded-square tile, with mint/cyan and periwinkle disk sectors.
- The open circular silhouette suggests a disk, the initial O and recovered
  space. The inset sectors refer to the application's storage sunburst.
- Broad shapes and no lettering keep the identity readable when reduced.
- The original generated pixels and alpha channel are preserved unchanged.

The image generation brief is recorded verbatim in `generation-prompt-v1.txt`.

## Native resources

- `omd.icns`: macOS bundle/Dock/Finder icon, with native iconset resolutions
  from 16 pixels through 512@2x (1024 pixels).
- `omd.ico`: Windows executable/Explorer icon, with 16, 24, 32, 48, 64, 128 and
  256-pixel RGBA images. `build.rs` embeds it in the Windows executable.
- `omd-window-64.rgba`: compile-time embedded 64×64 sRGB RGBA bytes for iced's
  window icon. This avoids a runtime image decoder or external image file.
- `omd-window-64.png`: matching preview of the window resource.
- `manifest.json`: master/derivative checksums, dimensions and exporter version.

macOS packaging places the ICNS in `Contents/Resources` and declares
`CFBundleIconFile` before signing. Windows packaging verifies every embedded
icon image against the ICO before creating a release ZIP. Native CI runs those
checks on the tested executable for each platform.

The derived assets are checked in, so normal Cargo builds, packaging and CI do
not need Pillow or iconutil. To regenerate them on macOS, use Python with Pillow
(the checked-in manifest records version 12.3.0) and run:

```sh
python3 scripts/generate_icons.py
```

The exporter downsamples the full approved canvas without cropping or changing
the design, creates the ICNS using Apple's `iconutil`, and preserves transparency.
Its output manifest makes source/derived updates reviewable together.
