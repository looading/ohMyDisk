"""Check inventory traversal using a tiny virtual filesystem, never user files."""

from contextlib import contextmanager
import importlib.util
from pathlib import Path
import stat
from types import SimpleNamespace
import unittest
from unittest.mock import patch


SPEC = importlib.util.spec_from_file_location(
    "scan_bench", Path(__file__).resolve().parents[1] / "benchmarks/scan_bench.py")
scan_bench = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(scan_bench)


def metadata(kind, inode, *, device=1, size=4096, links=1, reparse=False):
    mode = {"directory": stat.S_IFDIR, "file": stat.S_IFREG,
            "symlink": stat.S_IFLNK}[kind]
    return SimpleNamespace(st_mode=mode | 0o700, st_dev=device, st_ino=inode,
                           st_size=size, st_nlink=links, st_mtime_ns=0,
                           st_ctime_ns=0, st_file_attributes=0x400 if reparse else 0)


class InventoryTests(unittest.TestCase):
    def inventory(self, entries, information):
        # No directory is created or read. Unexpected descent fails immediately,
        # including on a cycle, rather than permitting a test to walk forever.
        root = Path("virtual-inventory-root")
        visited = []

        def key(path):
            return Path(path).relative_to(root).as_posix()

        def lstat(path):
            name = key(path)
            self.assertIn(name, information, "Unexpected metadata request")
            return information[name]

        @contextmanager
        def scandir(path):
            name = key(path)
            self.assertIn(name, entries, "A boundary directory was traversed")
            self.assertNotIn(name, visited, "A directory path was revisited")
            visited.append(name)
            yield [SimpleNamespace(name=child, path=str(Path(path) / child))
                   for child in entries[name]]

        with patch.object(Path, "lstat", lstat), patch.object(scan_bench.os, "scandir", scandir):
            result = scan_bench.inventory(root)
        return result, visited

    def test_directory_alias_is_retained_without_counting_its_contents_twice(self):
        for order in (["original", "alias"], ["alias", "original"]):
            with self.subTest(order=order):
                result, visited = self.inventory(
                    {".": order, "original": ["data"], "alias": ["data"]},
                    {".": metadata("directory", 1),
                     "original": metadata("directory", 2),
                     "alias": metadata("directory", 2),
                     "original/data": metadata("file", 3, size=23),
                     "alias/data": metadata("file", 3, size=23)})
                self.assertEqual((result["nodes"], result["directories"], result["files"]), (4, 3, 1))
                self.assertEqual(result["logical_bytes"], 23)
                self.assertEqual(result["boundaries"], 1)
                self.assertFalse(result["expected_complete"])
                self.assertEqual(len(visited), 2)

    def test_cycle_to_root_stops_at_the_repeated_directory_identity(self):
        result, visited = self.inventory(
            {".": ["child"], "child": ["to-root", "data"]},
            {".": metadata("directory", 1),
             "child": metadata("directory", 2),
             "child/to-root": metadata("directory", 1),
             "child/data": metadata("file", 3, size=7)})
        self.assertEqual(visited, [".", "child"])
        self.assertEqual((result["nodes"], result["directories"], result["files"]), (4, 3, 1))
        self.assertEqual(result["logical_bytes"], 7)
        self.assertEqual(result["boundaries"], 1)
        self.assertFalse(result["expected_complete"])

    def test_foreign_volume_boundary_does_not_hide_a_local_inode(self):
        result, visited = self.inventory(
            {".": ["local", "mount"], "local": ["data"]},
            {".": metadata("directory", 1),
             "local": metadata("directory", 2),
             "mount": metadata("directory", 2, device=2, size=1024),
             "local/data": metadata("file", 3, size=11)})
        self.assertEqual(visited, [".", "local"])
        self.assertEqual((result["nodes"], result["directories"], result["files"]), (4, 3, 1))
        # Existing cross-volume boundary semantics retain its native length;
        # the new zero-length rule applies only to a repeated local directory.
        self.assertEqual(result["logical_bytes"], 1035)
        self.assertEqual(result["boundaries"], 1)

    def test_regular_file_hardlinks_keep_logical_size_per_entry(self):
        result, visited = self.inventory(
            {".": ["left", "right"], "left": ["data"], "right": ["data"]},
            {".": metadata("directory", 1),
             "left": metadata("directory", 2),
             "right": metadata("directory", 3),
             "left/data": metadata("file", 4, size=17, links=2),
             "right/data": metadata("file", 4, size=17, links=2)})
        self.assertEqual(set(visited), {".", "left", "right"})
        self.assertEqual((result["nodes"], result["directories"], result["files"]), (5, 3, 2))
        self.assertEqual(result["logical_bytes"], 34)
        self.assertEqual(result["hardlinked_file_entries"], 2)
        self.assertEqual(result["boundaries"], 0)
        self.assertTrue(result["expected_complete"])

    def test_directory_links_and_reparse_points_are_still_not_followed(self):
        result, visited = self.inventory(
            {".": ["real", "link", "reparse"], "real": ["data"]},
            {".": metadata("directory", 1),
             "real": metadata("directory", 2),
             "link": metadata("symlink", 3, size=7),
             "reparse": metadata("directory", 2, size=9, reparse=True),
             "real/data": metadata("file", 4, size=17)})
        self.assertEqual(visited, [".", "real"])
        self.assertEqual((result["nodes"], result["directories"], result["files"]), (5, 2, 3))
        self.assertEqual(result["kinds"]["symlink"], 2)
        self.assertEqual(result["logical_bytes"], 33)
        self.assertEqual(result["boundaries"], 2)
        self.assertFalse(result["expected_complete"])


if __name__ == "__main__":
    unittest.main()
