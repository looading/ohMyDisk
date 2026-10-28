"""Verify release metadata and archive safety without running native executables."""

import hashlib
import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest
from unittest.mock import patch
import zipfile

SPEC = importlib.util.spec_from_file_location("package", Path(__file__).resolve().parents[1] / "scripts/package.py")
package = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(package)


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="ohmydisk-package-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n', encoding="utf-8")
        (self.root / "README.md").write_text("Package README", encoding="utf-8")
        (self.root / "docs").mkdir()
        (self.root / "docs/mcp.md").write_text("MCP instructions", encoding="utf-8")

    def executable(self, target, architecture):
        suffix = ".exe" if "windows" in target else ""
        binary = self.root / f"target/{target}/release/ohmydisk{suffix}"
        binary.parent.mkdir(parents=True)
        if suffix:
            header = bytearray(64)
            header[:2] = b"MZ"
            header[60:64] = (64).to_bytes(4, "little")
            binary.write_bytes(header + b"PE\x00\x00\x64\x86")
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
        self.assertEqual(archive.name, "ohMyDisk-windows-x86_64.zip")
        with zipfile.ZipFile(archive) as zipped:
            prefix = "ohMyDisk-windows-x86_64/"
            self.assertEqual(zipped.read(prefix + "ohmydisk.exe"), binary.read_bytes())
            self.assertEqual(zipped.read(prefix + "README.md"), b"Package README")
            self.assertEqual(zipped.read(prefix + "docs/mcp.md"), b"MCP instructions")
        self.assertEqual(checksum.read_text(encoding="utf-8"), f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")
        run.assert_not_called()

    @patch.object(package.platform, "system", return_value="Darwin")
    @patch.object(package.platform, "machine", return_value="arm64")
    @patch.object(package.subprocess, "run")
    def test_macos_bundle_uses_workspace_version_and_executable_mode(self, run, _machine, _system):
        target = "aarch64-apple-darwin"
        self.executable(target, "aarch64")
        archive, _ = package.package(self.root, skip_build=True, target=target)
        self.assertEqual(archive.name, "ohMyDisk-macos-aarch64.zip")
        with zipfile.ZipFile(archive) as zipped:
            prefix = "ohMyDisk-macos-aarch64/"
            metadata = plistlib.loads(zipped.read(prefix + "ohMyDisk.app/Contents/Info.plist"))
            self.assertEqual(metadata["CFBundleVersion"], "1.2.3")
            self.assertEqual(metadata["CFBundleShortVersionString"], "1.2.3")
            self.assertTrue(zipped.getinfo(prefix + "ohMyDisk.app/Contents/MacOS/ohmydisk").external_attr >> 16 & 0o111)
            self.assertIn(prefix + "docs/mcp.md", zipped.namelist())
        self.assertEqual(run.call_args_list[0].args[0][:5], ["codesign", "--force", "--deep", "--sign", "-"])
        self.assertEqual(run.call_args_list[1].args[0][:4], ["codesign", "--verify", "--deep", "--strict"])

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
