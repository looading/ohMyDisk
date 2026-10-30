# Native application icons — 2026-10-30

The user approved `omd-app-icon-v1.png` and requested application/packaging
integration on macOS and Windows. Product commit
`a00f99830e4952ea959d0c158649b7d274d0654f` implements that integration without
changing the approved artwork, scanner, cleanup flow or CLI/MCP subsystem.

## Installed resources

| Location | Resource and behavior |
|---|---|
| iced window settings | 64×64 sRGB RGBA pixels embedded at compile time; no external file or runtime image decoder |
| Windows executable | Icon resource group ID 1, seven image resolutions (16/24/32/48/64/128/256), inherited workspace version and ohMyDisk product metadata |
| Windows ZIP | The verified EXE plus the corresponding standalone `omd.ico` |
| macOS application bundle | `Contents/Resources/omd.icns`, declared by `CFBundleIconFile` before signing |
| Source repository | Approved master, native derivatives, checksums and an offline exporter under `assets/icons` / `scripts/generate_icons.py` |

Windows compilation fails if the resource compiler cannot embed the icon.
Packaging reads the resulting executable's PE resource tree and checks every
icon group and language variant against the expected metadata and exact image
payloads. It accepts resource-ID reordering and the compiler's equivalent
plane-0/plane-1 metadata while rejecting stale or missing representations.

Mac packaging copies the icon and writes the bundle declaration before ad hoc
signing, then verifies the signature. Developer ID signing/notarization remains
separate from this icon change.

## Verification

- Local validation passed 174 Rust tests, 15 Python tests, formatting, Clippy
  (with lint warnings), release build and isolated release MCP smoke workflows.
- Packaging tests cover missing images/groups, stale small/large images, wrong
  dimensions/bit depths/planes, invalid resource offsets, changed language
  variants, archive contents and presence of the bundle icon before signing.
- The new archive assertions fail against the previous packaging implementation
  and pass with this change.
- Native Mac packaging and strict signature verification passed. NSWorkspace
  resolved and rendered the new application icon from the signed local bundle.
- The local Mac ZIP passed SHA256, archive integrity, bundle icon declaration
  and exact ICNS-byte comparison against the checked-in asset.
- [Native CI run 40907202426](https://github.com/looading/ohMyDisk/actions/runs/40907202426)
  passed Windows MSVC, macOS Apple Silicon and macOS Intel, including the actual
  release executable icon verification, existing benchmark/MCP smoke and
  packaging. The release publication job was skipped for this PR run.

![System rendering of the signed local Mac application icon](../images/native-bundle-icon.png)

This system rendering is an artifact check, not a screenshot of a running app.
No Windows GUI appearance or native window gesture acceptance is claimed here;
the Windows evidence is the native executable/resource and packaging workflow.

## Asset fidelity and regeneration

The master SHA256 remains
`f42e6f25b1828740929b580af21a49b7f72d216ff17b15ade414488527c6383c`.
Every derivative matches `assets/icons/manifest.json`; the embedded RGBA bytes
match the 64-pixel PNG. All eight PNG representations inside the ICNS match the
source-derived RGBA pixels exactly.

The native 16/32-pixel ARGB representations preserve alpha and opaque colors
exactly. Semitransparent channels undergo native premultiply/unpremultiply
quantization, with a maximum visible channel difference of 1/255; quantized
premultiplied pixels are identical. These checks decode the ARGB resources
directly, since reverse conversion through iconutil is lossy for those blocks.
The [upstream icon tool notes](https://github.com/electron-userland/electron-builder-binaries/blob/master/packages/icons/CHANGELOG.md#123)
also describe this verification limitation.

Normal Rust builds and CI consume checked-in resources. Pillow and Apple's
iconutil are needed only to regenerate them. Both Cargo CI cache keys and the
Windows build script track icon assets, so changing the icon triggers a rebuild.

## Distribution and download boundary

The native run uploaded these complete artifacts (each includes its ZIP and
SHA256 file):

- [Windows x64](https://github.com/looading/ohMyDisk/actions/runs/40907202426/artifacts/12912925811)
- [macOS Apple Silicon](https://github.com/looading/ohMyDisk/actions/runs/40907202426/artifacts/12912471951)
- [macOS Intel](https://github.com/looading/ohMyDisk/actions/runs/40907202426/artifacts/12912267285)

Independent local re-download was limited by very low artifact-blob throughput.
Hanging download processes were stopped and fallback attempts were bounded.
Consequently, post-download CI-archive checksum/signature/payload checks are not
claimed as completed. This does not substitute for the native CI package checks
described above; it records the separate verification boundary.
