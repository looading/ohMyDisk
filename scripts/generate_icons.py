#!/usr/bin/env python3
"""Export the approved icon master for both platforms (macOS, Pillow 12.3.0)."""

import hashlib
import io
import json
from pathlib import Path
import shutil
import subprocess
import tempfile

from PIL import Image, __version__ as pillow_version


ROOT = Path(__file__).resolve().parents[1]
ICONS = ROOT / "assets/icons"
MASTER = ICONS / "omd-app-icon-v1.png"
ICO_SIZES = (16, 24, 32, 48, 64, 128, 256)


def main():
    iconutil = shutil.which("iconutil")
    if iconutil is None:
        raise SystemExit("Run this offline asset exporter on macOS with iconutil installed.")
    with Image.open(MASTER) as source:
        master = source.convert("RGBA")
    if master.width != master.height:
        raise ValueError("The approved icon master must be square")
    if master.getchannel("A").getextrema() != (0, 255):
        raise ValueError("The approved icon master must preserve transparent outer corners")

    def resized(size):
        return master.resize((size, size), Image.Resampling.LANCZOS)

    with tempfile.TemporaryDirectory(prefix="omd-icon-export-") as directory:
        iconset = Path(directory) / "omd.iconset"
        iconset.mkdir()
        for size in (16, 32, 128, 256, 512):
            for scale, suffix in ((1, ""), (2, "@2x")):
                resized(size * scale).save(iconset / f"icon_{size}x{size}{suffix}.png")
        subprocess.run([iconutil, "--convert", "icns", "--output", str(ICONS / "omd.icns"), str(iconset)], check=True)

    master.save(ICONS / "omd.ico", format="ICO", sizes=[(size, size) for size in ICO_SIZES])
    window = resized(64)
    window.save(ICONS / "omd-window-64.png")
    (ICONS / "omd-window-64.rgba").write_bytes(window.tobytes())

    # Keep every ICO image tied to the same approved master, rather than merely
    # checking the directory labels. This also detects accidental missing sizes.
    with Image.open(ICONS / "omd.ico") as icon:
        if icon.ico.sizes() != {(size, size) for size in ICO_SIZES}:
            raise ValueError("ICO does not contain every required resolution")
        for size in ICO_SIZES:
            actual = icon.ico.getimage((size, size)).convert("RGBA")
            expected = Image.open(io.BytesIO(_ico_image(master, size))).convert("RGBA")
            if actual.tobytes() != expected.tobytes():
                raise ValueError(f"ICO pixels differ from the approved master at {size}px")

    names = (MASTER.name, "omd.icns", "omd.ico", "omd-window-64.png", "omd-window-64.rgba")
    manifest = {
        "source": MASTER.name,
        "source_size": [master.width, master.height],
        "pillow_version": pillow_version,
        "ico_sizes": list(ICO_SIZES),
        "window_rgba_size": [64, 64],
        "sha256": {name: hashlib.sha256((ICONS / name).read_bytes()).hexdigest() for name in names},
    }
    (ICONS / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(manifest, indent=2))


def _ico_image(master, size):
    """Pillow's ICO writer uses thumbnail to preserve the source aspect ratio."""
    image = master.copy()
    image.thumbnail((size, size), Image.Resampling.LANCZOS, reducing_gap=None)
    data = io.BytesIO()
    image.save(data, format="PNG")
    return data.getvalue()


if __name__ == "__main__":
    main()
