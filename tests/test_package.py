"""Verify release metadata and archive safety without running native executables."""

import hashlib
import importlib.util
import copy
from pathlib import Path
import plistlib
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile

SPEC = importlib.util.spec_from_file_location("package", Path(__file__).resolve().parents[1] / "scripts/package.py")
package = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package)


def pe_with_resources(resources):
    """Create a PE32+ fixture with resource RVAs different from file offsets."""
    resource_rva = 0x3000
    resource_data = bytearray()

    def allocate(size):
        resource_data.extend(bytes(-len(resource_data) % 4))
        offset = len(resource_data)
        resource_data.extend(bytes(size))
        return offset

    def directory(children):
        offset = allocate(16 + len(children) * 8)
        struct.pack_into("<HH", resource_data, offset + 12, 0, len(children))
        for index, (key, value) in enumerate(sorted(children.items())):
            if isinstance(value, dict):
                destination = directory(value) | 0x80000000
            else:
                destination = allocate(16)
                payload = allocate(len(value))
                resource_data[payload:payload + len(value)] = value
                struct.pack_into("<IIII", resource_data, destination,
                                 resource_rva + payload, len(value), 0, 0)
            struct.pack_into("<II", resource_data, offset + 16 + index * 8, key, destination)
        return offset

    directory(resources)
    image = bytearray(0x200)
    image[:2] = b"MZ"
    struct.pack_into("<I", image, 60, 0x80)
    image[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HHIIIHH", image, 0x84, 0x8664, 1, 0, 0, 0, 240, 2)
    optional = 0x98
    struct.pack_into("<H", image, optional, 0x20B)
    struct.pack_into("<I", image, optional + 108, 16)
    struct.pack_into("<II", image, optional + 128, resource_rva, len(resource_data))
    section = optional + 240
    image[section:section + 8] = b".rsrc\0\0\0"
    struct.pack_into("<IIII", image, section + 8,
                     len(resource_data), resource_rva, len(resource_data), 0x200)
    return bytes(image + resource_data)


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ohmydisk-package-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
        (self.root / "README.md").write_text("Package README", encoding="utf-8")
        (self.root / "docs").mkdir()
        (self.root / "docs/mcp.md").write_text("MCP instructions", encoding="utf-8")
        icons = self.root / "assets/icons"
        icons.mkdir(parents=True)
        self.icon = icons / "omd.ico"
        self.images = [
            (struct.pack("<BBBBHHI", 16, 16, 0, 0, 0, 32, 14), b"small approved"),
            (struct.pack("<BBBBHHI", 0, 0, 0, 0, 0, 32, 14), b"large approved"),
        ]
        payload_offset = 6 + len(self.images) * 16
        ico = bytearray(struct.pack("<HHH", 0, 1, len(self.images)))
        for metadata, payload in self.images:
            ico.extend(metadata + struct.pack("<I", payload_offset))
            payload_offset += len(payload)
        self.icon.write_bytes(ico + b"".join(payload for _, payload in self.images))
        self.icns = icons / "omd.icns"
        self.icns.write_bytes(b"icns\0\0\0\x08")

    def icon_resources(self):
        # Icon resource IDs need not be consecutive or ordered like the ICO.
        image_ids = (9, 3)
        group = struct.pack("<HHH", 0, 1, len(self.images)) + b"".join(
            metadata[:4] + struct.pack("<H", 1) + metadata[6:] + struct.pack("<H", image_id)
            for image_id, (metadata, _) in zip(image_ids, self.images)
        )
        return {
            3: {image_id: {1033: payload} for image_id, (_, payload) in zip(image_ids, self.images)},
            14: {37: {1033: group}},
        }

    def executable(self, target, architecture, resources=None):
        suffix = ".exe" if "windows" in target else ""
        binary = self.root / f"target/{target}/release/ohmydisk{suffix}"
        binary.parent.mkdir(parents=True)
        if suffix:
            binary.write_bytes(pe_with_resources(self.icon_resources() if resources is None else resources))
        else:
            cpu = {"x86_64": 0x01000007, "aarch64": 0x0100000C}[architecture]
            binary.write_bytes(b"\xcf\xfa\xed\xfe" + cpu.to_bytes(4, "little") + bytes(56))
        binary.chmod(0o755)
        return binary

    def test_tag_must_exactly_match_workspace_version(self):
        version = package.workspace_version(self.root)
        self.assertEqual(version, "1.2.3")
        package.validate_tag("v1.2.3", version)
        package.validate_tag("", version)
        for tag in ("v1.2.4", "1.2.3", "v1.2.3-extra"):
            with self.subTest(tag=tag), self.assertRaises(ValueError):
                package.validate_tag(tag, version)

    def test_host_architecture_aliases_are_stable(self):
        self.assertEqual(package.native_target("Windows", "AMD64"), "x86_64-pc-windows-msvc")
        self.assertEqual(package.native_target("Darwin", "arm64"), "aarch64-apple-darwin")
        with self.assertRaises(ValueError):
            package.native_target("Linux", "x86_64")

    @patch.object(package.platform, "system", return_value="Windows")
    @patch.object(package.platform, "machine", return_value="AMD64")
    @patch.object(package.subprocess, "run")
    def test_windows_archive_docs_checksum_and_no_rebuild(self, run, _machine, _system):
        target = "x86_64-pc-windows-msvc"
        binary = self.executable(target, "x86_64")
        archive, checksum = package.package(self.root, skip_build=True, target=target)
        self.assertEqual(archive.name, "OhMyDisk-windows-x86_64.zip")
        with zipfile.ZipFile(archive) as zipped:
            prefix = "OhMyDisk-windows-x86_64/"
            self.assertEqual(zipped.read(prefix + "ohmydisk.exe"), binary.read_bytes())
            self.assertEqual(zipped.read(prefix + "omd.ico"), self.icon.read_bytes())
            self.assertEqual(zipped.read(prefix + "README.md"), b"Package README")
            self.assertEqual(zipped.read(prefix + "docs/mcp.md"), b"MCP instructions")
        self.assertEqual(checksum.read_text(encoding="utf-8"), f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
        run.assert_not_called()

    @patch.object(package.platform, "system", return_value="Darwin")
    @patch.object(package.platform, "machine", return_value="arm64")
    @patch.object(package.subprocess, "run")
    def test_macos_bundle_uses_workspace_version_and_executable_mode(self, run, _machine, _system):
        target = "aarch64-apple-darwin"
        binary = self.executable(target, "aarch64")
        dist = self.root / "dist"
        dist.mkdir()
        # Rebuilding after a case-only rename must update actual directory
        # entries on the case-insensitive filesystems used by macOS/Windows.
        (dist / "ohMyDisk-macos-aarch64.zip").write_bytes(b"old archive")
        (dist / "ohMyDisk-macos-aarch64.zip.sha256").write_bytes(b"old checksum")

        def check_icon_before_signing(command, **_kwargs):
            contents = Path(command[-1]) / "Contents"
            metadata = plistlib.loads((contents / "Info.plist").read_bytes())
            self.assertEqual(metadata["CFBundleIconFile"], "omd.icns")
            self.assertEqual((contents / "Resources" / metadata["CFBundleIconFile"]).read_bytes(), self.icns.read_bytes())

        run.side_effect = check_icon_before_signing
        archive, _ = package.package(self.root, skip_build=True, target=target)
        self.assertEqual(archive.name, "OhMyDisk-macos-aarch64.zip")
        self.assertTrue({"OhMyDisk-macos-aarch64.zip", "OhMyDisk-macos-aarch64.zip.sha256"} <= {p.name for p in dist.iterdir()})
        with zipfile.ZipFile(archive) as zipped:
            prefix = "OhMyDisk-macos-aarch64/"
            metadata = plistlib.loads(zipped.read(prefix + "OhMyDisk.app/Contents/Info.plist"))
            self.assertEqual(metadata["CFBundleName"], "OhMyDisk")
            self.assertEqual(metadata["CFBundleDisplayName"], "OhMyDisk")
            self.assertEqual(metadata["CFBundleVersion"], "1.2.3")
            self.assertEqual(metadata["CFBundleShortVersionString"], "1.2.3")
            self.assertEqual(metadata["CFBundleIconFile"], "omd.icns")
            self.assertEqual(zipped.read(prefix + "OhMyDisk.app/Contents/Resources/omd.icns"), self.icns.read_bytes())
            # The simulation also runs on Windows, whose chmod cannot create
            # POSIX execute bits. The archive must preserve its actual source mode.
            self.assertEqual(
                zipped.getinfo(prefix + "OhMyDisk.app/Contents/MacOS/ohmydisk").external_attr >> 16 & 0o111,
                binary.stat().st_mode & 0o111,
            )
            self.assertIn(prefix + "docs/mcp.md", zipped.namelist())
        self.assertEqual(run.call_args_list[0].args[0][:5], ["codesign", "--force", "--deep", "--sign", "-"])
        self.assertEqual(run.call_args_list[1].args[0][:4], ["codesign", "--verify", "--deep", "--strict"])

    def test_windows_icon_resources_preserve_every_approved_image(self):
        binary = self.executable("x86_64-pc-windows-msvc", "x86_64")
        package.verify_windows_icon(binary, self.icon)
        # Resource order is irrelevant; the group resolves images by their IDs.
        resources = self.icon_resources()
        group = resources[14][37][1033]
        resources[14][37][1033] = group[:6] + group[20:34] + group[6:20]
        binary.write_bytes(pe_with_resources(resources))
        package.verify_windows_icon(binary, self.icon)

    def test_windows_icon_verification_rejects_stale_missing_and_inconsistent_resources(self):
        original = self.icon_resources()
        cases = {}
        cases["missing group"] = {3: original[3]}
        cases["missing image"] = copy.deepcopy(original)
        del cases["missing image"][3][9]
        cases["stale small image"] = copy.deepcopy(original)
        cases["stale small image"][3][9][1033] = b"old small icon"
        cases["stale large image"] = copy.deepcopy(original)
        cases["stale large image"][3][3][1033] = b"old large icon"
        cases["wrong size metadata"] = copy.deepcopy(original)
        group = original[14][37][1033]
        cases["wrong size metadata"][14][37][1033] = group[:6] + b"\x20" + group[7:]
        cases["wrong bit depth"] = copy.deepcopy(original)
        cases["wrong bit depth"][14][37][1033] = group[:12] + struct.pack("<H", 8) + group[14:]
        cases["wrong planes"] = copy.deepcopy(original)
        cases["wrong planes"][14][37][1033] = group[:10] + struct.pack("<H", 2) + group[12:]
        cases["stale language variant"] = copy.deepcopy(original)
        cases["stale language variant"][3][9][0] = b"old small icon"
        cases["stale second group"] = copy.deepcopy(original)
        cases["stale second group"][14][1] = {1033: group[:6] + b"\x20" + group[7:]}
        binary = self.executable("x86_64-pc-windows-msvc", "x86_64")
        for label, resources in cases.items():
            with self.subTest(case=label):
                binary.write_bytes(pe_with_resources(resources))
                with self.assertRaisesRegex(ValueError, "Windows icon verification failed"):
                    package.verify_windows_icon(binary, self.icon)

    def test_windows_icon_verification_rejects_malformed_resource_offsets(self):
        binary = self.executable("x86_64-pc-windows-msvc", "x86_64")
        contents = bytearray(binary.read_bytes())
        # Point the first resource type directory beyond .rsrc's declared size.
        struct.pack_into("<I", contents, 0x200 + 20, 0x80000000 | len(contents))
        binary.write_bytes(contents)
        with self.assertRaisesRegex(ValueError, "resource directory is out of bounds"):
            package.verify_windows_icon(binary, self.icon)

    @patch.object(package.platform, "system", return_value="Windows")
    @patch.object(package.platform, "machine", return_value="AMD64")
    def test_windows_package_rejects_missing_icon_before_creating_distribution(self, _machine, _system):
        target = "x86_64-pc-windows-msvc"
        self.executable(target, "x86_64", {24: {1: {1033: b"manifest"}}})
        with self.assertRaisesRegex(ValueError, "missing embedded icon resources"):
            package.package(self.root, skip_build=True, target=target)
        self.assertFalse((self.root / "dist").exists())

    @patch.object(package.platform, "system", return_value="Darwin")
    @patch.object(package.platform, "machine", return_value="arm64")
    def test_mismatched_runner_or_binary_is_rejected(self, _machine, _system):
        with self.assertRaisesRegex(ValueError, "native runner"):
            package.package(self.root, skip_build=True, target="x86_64-apple-darwin")
        target = "aarch64-apple-darwin"
        self.executable(target, "x86_64")
        with self.assertRaisesRegex(ValueError, "Executable architecture"):
            package.package(self.root, skip_build=True, target=target)
        self.assertFalse((self.root / "dist").exists())

    @patch.object(package.platform, "system", return_value="Windows")
    @patch.object(package.platform, "machine", return_value="AMD64")
    @patch.object(package.subprocess, "run")
    def test_build_is_locked_and_has_explicit_target(self, run, _machine, _system):
        target = "x86_64-pc-windows-msvc"
        self.executable(target, "x86_64")
        package.package(self.root, target=target)
        self.assertEqual(run.call_args.args[0], ["cargo", "build", "--locked", "-p", "ohmydisk", "--release", "--target", target])


if __name__ == "__main__":
    unittest.main()
