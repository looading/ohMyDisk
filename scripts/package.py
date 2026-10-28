#!/usr/bin/env python3
"""Build and package a native desktop distribution (Python 3.11 or newer)."""

import argparse
import hashlib
import os
from pathlib import Path
import platform
import plistlib
import shutil
import subprocess
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "aarch64-apple-darwin": ("Darwin", "aarch64", "macos", ""),
    "x86_64-apple-darwin": ("Darwin", "x86_64", "macos", ""),
    "x86_64-pc-windows-msvc": ("Windows", "x86_64", "windows", ".exe"),
}


def workspace_version(root):
    with (root / "Cargo.toml").open("rb") as source:
        return tomllib.load(source)["workspace"]["package"]["version"]


def validate_tag(tag, version):
    if tag and tag != f"v{version}":
        raise ValueError(f"Release tag {tag!r} must equal workspace version v{version}")


def native_target(system, machine):
    architecture = {"arm64": "aarch64", "amd64": "x86_64"}.get(machine.lower(), machine.lower())
    for target, (target_system, target_architecture, _, _) in TARGETS.items():
        if (system, architecture) == (target_system, target_architecture):
            return target
    raise ValueError(f"Packaging is unsupported on {system}/{machine}")


def binary_architecture(binary, system):
    """Read the executable header so a stale build cannot be mislabeled."""
    with binary.open("rb") as source:
        header = source.read(64)
        if system == "Darwin" and header[:4] in (b"\xcf\xfa\xed\xfe", b"\xfe\xed\xfa\xcf"):
            byteorder = "little" if header[:4] == b"\xcf\xfa\xed\xfe" else "big"
            cpu = int.from_bytes(header[4:8], byteorder)
            architecture = {0x01000007: "x86_64", 0x0100000C: "aarch64"}.get(cpu)
        elif system == "Windows" and header[:2] == b"MZ" and len(header) == 64:
            source.seek(int.from_bytes(header[60:64], "little"))
            pe_header = source.read(6)
            architecture = "x86_64" if pe_header == b"PE\x00\x00\x64\x86" else None
        else:
            architecture = None
    if architecture is None:
        raise ValueError(f"Unsupported or invalid native executable: {binary}")
    return architecture


def package(root, *, debug=False, skip_build=False, target=None):
    system = platform.system()
    host_target = native_target(system, platform.machine())
    if target is not None and target != host_target:
        raise ValueError(f"Target {target} does not match this native runner ({host_target})")
    selected_target = target or host_target
    _, architecture, os_label, extension = TARGETS[selected_target]
    profile = "debug" if debug else "release"
    env = os.environ.copy()
    if system == "Darwin":
        env["MACOSX_DEPLOYMENT_TARGET"] = "13.0"
    if not skip_build:
        command = ["cargo", "build", "--locked", "-p", "ohmydisk"]
        if not debug:
            command.append("--release")
        if target:
            command.extend(["--target", target])
        subprocess.run(command, cwd=root, env=env, check=True)
    # An explicit --target uses Cargo's target-specific output directory.
    binary = root / "target"
    if target:
        binary /= target
    binary /= f"{profile}/ohmydisk{extension}"
    if binary_architecture(binary, system) != architecture:
        raise ValueError(f"Executable architecture does not match target {selected_target}: {binary}")

    version = workspace_version(root)
    dist = root / "dist"
    name = f"ohMyDisk-{os_label}-{architecture}"
    folder = dist / name
    if folder.exists():
        shutil.rmtree(folder)
    (folder / "docs").mkdir(parents=True)
    shutil.copy2(root / "README.md", folder / "README.md")
    shutil.copy2(root / "docs/mcp.md", folder / "docs/mcp.md")
    if (root / "LICENSE").is_file():
        shutil.copy2(root / "LICENSE", folder / "LICENSE")

    if system == "Darwin":
        contents = folder / "ohMyDisk.app/Contents"
        (contents / "MacOS").mkdir(parents=True)
        shutil.copy2(binary, contents / "MacOS/ohmydisk")
        bundle_version = version.split("-", 1)[0].split("+", 1)[0]
        with (contents / "Info.plist").open("wb") as out:
            plistlib.dump({
                "CFBundleName": "ohMyDisk", "CFBundleDisplayName": "ohMyDisk",
                "CFBundleIdentifier": "app.ohmydisk.desktop", "CFBundleExecutable": "ohmydisk",
                "CFBundlePackageType": "APPL", "CFBundleVersion": bundle_version,
                "CFBundleShortVersionString": bundle_version, "LSMinimumSystemVersion": "13.0",
                "NSHighResolutionCapable": True,
                "NSDocumentsFolderUsageDescription": "ohMyDisk scans only the folders you choose to analyze disk usage.",
                "NSDownloadsFolderUsageDescription": "ohMyDisk analyzes selected files and folders and executes reviewed cleanup plans.",
                "NSDesktopFolderUsageDescription": "ohMyDisk analyzes selected Desktop folders.",
            }, out)
        # Ad-hoc signing permits local launch; distribution signing/notarization is separate.
        subprocess.run(["codesign", "--force", "--deep", "--sign", "-", str(contents.parent)], check=True)
        subprocess.run(["codesign", "--verify", "--deep", "--strict", str(contents.parent)], check=True)
    else:
        shutil.copy2(binary, folder / "ohmydisk.exe")

    archive = dist / f"{name}.zip"
    with zipfile.ZipFile(archive, "w", compression=zipfile.ZIP_DEFLATED) as output:
        for source in sorted(folder.rglob("*")):
            output.write(source, source.relative_to(dist))
    checksum = archive.with_suffix(".zip.sha256")
    with archive.open("rb") as source:
        digest = hashlib.file_digest(source, "sha256").hexdigest()
    checksum.write_text(f"{digest}  {archive.name}\n", encoding="utf-8", newline="\n")
    return archive, checksum


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--debug", action="store_true", help="Package a development build")
    parser.add_argument("--skip-build", action="store_true", help="Use an existing executable")
    parser.add_argument("--target", choices=TARGETS, help="Native Rust target; uses target/<target>/<profile>")
    parser.add_argument("--tag", default="", help="Require a release tag matching the workspace version")
    parser.add_argument("--check-version-only", action="store_true", help="Check/print version without building or packaging")
    options = parser.parse_args()
    try:
        version = workspace_version(ROOT)
        validate_tag(options.tag, version)
        if options.check_version_only:
            print(version)
            return
        for artifact in package(ROOT, debug=options.debug, skip_build=options.skip_build, target=options.target):
            print(artifact)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"Packaging failed: {error}\n")


if __name__ == "__main__":
    main()
