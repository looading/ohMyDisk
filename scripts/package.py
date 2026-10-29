#!/usr/bin/env python3
"""Build and package a native desktop distribution (Python 3.11 or newer)."""

import argparse
from collections import Counter
import hashlib
import os
from pathlib import Path
import platform
import plistlib
import shutil
import struct
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


def windows_icon_resources(binary):
    """Read RT_ICON / RT_GROUP_ICON resources without loading the executable."""
    data = binary.read_bytes()

    def read(offset, size):
        if offset < 0 or size < 0 or offset + size > len(data):
            raise ValueError("truncated PE resource data")
        return data[offset:offset + size]

    def unpack(format, offset):
        return struct.unpack(format, read(offset, struct.calcsize(format)))

    if read(0, 2) != b"MZ":
        raise ValueError("invalid DOS header")
    pe, = unpack("<I", 60)
    if read(pe, 4) != b"PE\0\0":
        raise ValueError("invalid PE header")
    sections, = unpack("<H", pe + 6)
    optional_size, = unpack("<H", pe + 20)
    optional = pe + 24
    magic, = unpack("<H", optional)
    directories = {0x10B: 96, 0x20B: 112}.get(magic)
    if directories is None or optional_size < directories + 24:
        raise ValueError("invalid PE optional header")
    directory_count, = unpack("<I", optional + directories - 4)
    if directory_count < 3:
        raise ValueError("missing PE resource directory")
    resource_rva, resource_size = unpack("<II", optional + directories + 16)
    if not resource_rva or not resource_size:
        raise ValueError("missing embedded icon resources")

    section_table = optional + optional_size
    section_ranges = []
    for index in range(sections):
        virtual_size, virtual_address, raw_size, raw_offset = unpack("<IIII", section_table + index * 40 + 8)
        section_ranges.append((virtual_address, virtual_size, raw_size, raw_offset))

    def file_offset(rva, size):
        for address, virtual_size, raw_size, offset in section_ranges:
            relative = rva - address
            if 0 <= relative < max(virtual_size, raw_size) and relative + size <= raw_size:
                read(offset + relative, size)
                return offset + relative
        raise ValueError("PE resource RVA is outside file-backed sections")

    resource_base = file_offset(resource_rva, resource_size)

    def resource_read(offset, size):
        if offset < 0 or offset + size > resource_size:
            raise ValueError("PE resource directory is out of bounds")
        return read(resource_base + offset, size)

    def entries(offset):
        header = resource_read(offset, 16)
        named, numbered = struct.unpack_from("<HH", header, 12)
        table = resource_read(offset + 16, (named + numbered) * 8)
        return list(struct.iter_unpack("<II", table))

    def directory(offset):
        if not offset & 0x80000000:
            raise ValueError("PE icon resource is missing a directory")
        return entries(offset & 0x7FFFFFFF)

    resources = {}
    for resource_type, type_offset in entries(0):
        if resource_type not in (3, 14):
            continue
        names = resources.setdefault(resource_type, {})
        for name, name_offset in directory(type_offset):
            languages = names.setdefault(name, {})
            for language, data_offset in directory(name_offset):
                if data_offset & 0x80000000 or language in languages:
                    raise ValueError("invalid PE icon resource language entry")
                rva, size, _, _ = struct.unpack("<IIII", resource_read(data_offset, 16))
                languages[language] = read(file_offset(rva, size), size)
            if not languages:
                raise ValueError("PE icon resource has no language entries")
    if not resources.get(3) or not resources.get(14):
        raise ValueError("missing embedded icon resources")
    return resources


def verify_windows_icon(binary, icon):
    """Require every Windows icon group to contain the approved ICO's images."""
    def metadata(entry):
        # ICO writers can use unspecified planes (0). Resource compilers may
        # resolve that to the standard single plane while preserving the image.
        planes = entry[4:6]
        return entry[:4] + (b"\x01\x00" if planes == b"\x00\x00" else planes) + entry[6:12]

    try:
        expected = icon.read_bytes()
        if len(expected) < 6:
            raise ValueError("truncated source ICO header")
        reserved, kind, count = struct.unpack_from("<HHH", expected)
        if (reserved, kind) != (0, 1) or not count or len(expected) < 6 + count * 16:
            raise ValueError("invalid source ICO directory")
        images = []
        for index in range(count):
            offset = 6 + index * 16
            size, image_offset = struct.unpack_from("<II", expected, offset + 8)
            if not size or image_offset < 6 + count * 16 or image_offset + size > len(expected):
                raise ValueError("source ICO image is out of bounds")
            images.append((metadata(expected[offset:offset + 12]), expected[image_offset:image_offset + size]))
        expected_images = Counter(images)
        resources = windows_icon_resources(binary)
        for groups in resources[14].values():
            for group in groups.values():
                if len(group) < 6:
                    raise ValueError("truncated embedded icon group")
                reserved, kind, count = struct.unpack_from("<HHH", group)
                if (reserved, kind) != (0, 1) or not count or len(group) != 6 + count * 14:
                    raise ValueError("invalid embedded icon group")
                actual_images = []
                for index in range(count):
                    offset = 6 + index * 14
                    image_id, = struct.unpack_from("<H", group, offset + 12)
                    variants = resources[3].get(image_id)
                    if not variants:
                        raise ValueError(f"embedded icon group references missing image {image_id}")
                    payload = next(iter(variants.values()))
                    if any(image != payload for image in variants.values()):
                        raise ValueError(f"embedded icon image {image_id} differs between languages")
                    actual_images.append((metadata(group[offset:offset + 12]), payload))
                if Counter(actual_images) != expected_images:
                    raise ValueError("embedded icon images do not match the approved ICO")
    except ValueError as error:
        raise ValueError(f"Windows icon verification failed for {binary}: {error}") from error


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
    icon = root / "assets/icons" / ("omd.icns" if system == "Darwin" else "omd.ico")
    if system == "Windows":
        verify_windows_icon(binary, icon)

    version = workspace_version(root)
    dist = root / "dist"
    name = f"ohMyDisk-{os_label}-{architecture}"
    folder = dist / name
    if folder.exists():
        shutil.rmtree(folder)
    (folder / "docs").mkdir(parents=True)
    shutil.copy2(root / "README.md", folder / "README.md")
    shutil.copy2(root / "docs/mcp.md", folder / "docs/mcp.md")
    if (root / "docs/images").is_dir():
        shutil.copytree(root / "docs/images", folder / "docs/images")
    if (root / "LICENSE").is_file():
        shutil.copy2(root / "LICENSE", folder / "LICENSE")

    if system == "Darwin":
        contents = folder / "ohMyDisk.app/Contents"
        (contents / "MacOS").mkdir(parents=True)
        (contents / "Resources").mkdir()
        shutil.copy2(binary, contents / "MacOS/ohmydisk")
        shutil.copy2(icon, contents / "Resources/omd.icns")
        bundle_version = version.split("-", 1)[0].split("+", 1)[0]
        with (contents / "Info.plist").open("wb") as out:
            plistlib.dump({
                "CFBundleName": "ohMyDisk", "CFBundleDisplayName": "ohMyDisk",
                "CFBundleIdentifier": "app.ohmydisk.desktop", "CFBundleExecutable": "ohmydisk",
                "CFBundleIconFile": "omd.icns",
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
        shutil.copy2(icon, folder / "omd.ico")

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
