#!/usr/bin/env python3
"""Repeatable warm-cache core scan benchmarks (Python 3.11+, no dependencies).

The runner contract is documented in benchmarks/SCAN_BENCH.md. This script never
clears OS caches, builds binaries, starts the service, or scans a default user
directory. Fixture creation and independent inventory checks are outside scan
timings. Only aggregate values and binary hashes enter the JSON report.
"""

from __future__ import annotations

import argparse
from collections import Counter
from contextlib import ExitStack
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import random
import re
import stat
import statistics
import subprocess
import sys
import tempfile
import time


FIXTURES = ("small-files", "wide-directory", "deep-tree", "mixed")
NUMERIC_FIELDS = (
    "scan_ms", "serialize_ms", "snapshot_bytes", "nodes", "files", "directories",
    "logical_bytes", "metadata_calls", "peak_rss_bytes", "cancel_latency_ms",
    "metadata_ms", "progress_callbacks", "first_children_ms", "first_descendants_ms",
    "directory_calls", "directory_ms", "index_build_ms",
)
INTEGER_FIELDS = {
    "snapshot_bytes", "nodes", "files", "directories", "logical_bytes",
    "metadata_calls", "peak_rss_bytes",
    "progress_callbacks",
    "directory_calls",
}
REQUIRED_FIELDS = {
    "scan_ms", "serialize_ms", "snapshot_bytes", "nodes", "files", "directories",
    "logical_bytes", "complete", "cancelled", "validation_digest",
}


class BenchError(Exception):
    """Messages use fixed labels and counts, never filesystem paths."""


def elapsed_ms(start: float) -> float:
    return (time.perf_counter() - start) * 1000


def file_hash(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def write_file(path: Path, size: int, payload: bytes) -> None:
    with path.open("wb") as output:
        output.write(payload[:size])


def make_fixture(root: Path, name: str, args: argparse.Namespace) -> dict:
    """Use small actual writes rather than sparse files or hardlinks for bulk data."""
    seed = args.seed + FIXTURES.index(name)
    rng = random.Random(seed)
    payload = bytes(rng.randrange(256) for _ in range(4096))
    features = {"symlinks": 0, "hardlinks": 0, "symlinks_unavailable": False,
                "hardlinks_unavailable": False}
    if name == "small-files":
        count = args.small_files
        # Bound each directory to 1,000 files, isolating tree-walk work from the
        # deliberately wide fixture. Names and sizes are deterministic.
        for batch in range((count + 999) // 1000):
            directory = root / f"d{batch:06d}"
            directory.mkdir()
            for index in range(batch * 1000, min(count, (batch + 1) * 1000)):
                write_file(directory / f"f{index:09d}", 128, payload)
        parameters = {"regular_files": count, "file_bytes": 128,
                      "files_per_directory": 1000}
    elif name == "wide-directory":
        count = args.wide_files
        for index in range(count):
            write_file(root / f"f{index:09d}", 32, payload)
        parameters = {"regular_files": count, "file_bytes": 32}
    elif name == "deep-tree":
        directory = root
        for depth in range(args.deep_depth):
            directory /= "d"
            directory.mkdir()
            for index in range(args.deep_files_per_level):
                # Preserve the original single-file fixture byte-for-byte when
                # the new option is left at its default.
                filename = "f" if index == 0 else f"f{index:06d}"
                write_file(directory / filename, 128 + depth + index % 1024, payload)
        parameters = {"depth": args.deep_depth,
                      "files_per_level": args.deep_files_per_level,
                      "regular_files": args.deep_depth * args.deep_files_per_level,
                      "file_bytes_min": 128,
                      "file_bytes_max": 127 + args.deep_depth
                      + min(args.deep_files_per_level - 1, 1023)}
    else:
        directories = [root]
        for index in range(64):
            name_part = f".hidden-{index:03d}" if index % 8 == 0 else f"d{index:03d}"
            directory = root / name_part
            directory.mkdir()
            directories.append(directory)
        # Include empty, hidden, space-containing and non-ASCII entries without
        # relying on host-specific case-sensitive filename behavior.
        (root / "empty directory").mkdir()
        (root / ".hidden-empty").mkdir()
        for index in range(args.mixed_files):
            directory = directories[rng.randrange(len(directories))]
            prefix = ".hidden" if index % 7 == 0 else "file"
            filename = f"{prefix}-{index:09d}" + (" space-é" if index % 29 == 0 else "")
            write_file(directory / filename, rng.choice((0, 1, 17, 128, 1024, 4096)), payload)
        target = root / "link-target"
        write_file(target, 257, payload)
        try:
            os.link(target, root / "hardlink-target")
            features["hardlinks"] += 1
        except (OSError, NotImplementedError):
            features["hardlinks_unavailable"] = True
        # Relative targets keep fixture structure identical under different
        # temporary paths. Include a directory cycle and a dangling link; neither
        # should be followed by the scanner.
        for link_name, destination, is_directory in (
            ("file-link", "link-target", False),
            ("directory-link", "d001", True),
            ("cycle-link", ".", True),
            ("dangling-link", "missing-target", False),
        ):
            try:
                os.symlink(destination, root / link_name, target_is_directory=is_directory)
                features["symlinks"] += 1
            except (OSError, NotImplementedError):
                features["symlinks_unavailable"] = True
        parameters = {"regular_files_requested": args.mixed_files,
                      "top_level_directories": 66, "extra_target_file_bytes": 257}
    return {"kind": name, "seed": seed, "parameters": parameters, "features": features}


def is_reparse(info: os.stat_result) -> bool:
    return bool(getattr(info, "st_file_attributes", 0)
                & getattr(stat, "FILE_ATTRIBUTE_REPARSE_POINT", 0x400))


def inventory(root: Path) -> dict:
    """Independently enumerate entries without following links or crossing devices.

    NativePlatform and Python lstat both count hardlinked directory entries at
    full logical size. File contents, owners, absolute paths and inode values are
    never retained in the report. The transient change digest catches ordinary
    changes to real directories while measurements are running.
    """
    try:
        initial = root.lstat()
    except OSError:
        raise BenchError("Cannot inspect selected root") from None
    if not stat.S_ISDIR(initial.st_mode) or stat.S_ISLNK(initial.st_mode) or is_reparse(initial):
        raise BenchError("Selected root must be a real directory")
    counts: Counter = Counter()
    rows = []
    changes = []
    pending = [(root, b".")]
    logical_bytes = 0
    while pending:
        path, relative = pending.pop()
        try:
            info = path.lstat()
        except OSError:
            counts["metadata_errors"] += 1
            continue
        link = stat.S_ISLNK(info.st_mode) or is_reparse(info)
        if link:
            kind = "symlink"
        elif stat.S_ISDIR(info.st_mode):
            kind = "directory"
        elif stat.S_ISREG(info.st_mode):
            kind = "file"
        else:
            kind = "other"
        boundary = link or info.st_dev != initial.st_dev or kind == "other"
        directory = kind == "directory" and not boundary
        size = 0 if directory else info.st_size
        counts["nodes"] += 1
        counts["directories" if kind == "directory" else "files"] += 1
        counts[f"kind_{kind}"] += 1
        counts["boundaries"] += int(boundary)
        counts["hidden_entries"] += int(relative != b"." and path.name.startswith("."))
        counts["hardlinked_file_entries"] += int(kind == "file" and info.st_nlink > 1)
        logical_bytes += size
        # Length-prefix paths so arbitrary names cannot collide in the digest.
        row = len(relative).to_bytes(8, "little") + relative + b"\0" + kind.encode() + b"\0"
        rows.append((relative, row + str(size).encode() + b"\0" + bytes([boundary])))
        changes.append((relative, row + repr((info.st_dev, info.st_ino, info.st_size,
                                              info.st_mtime_ns, info.st_ctime_ns)).encode()))
        if directory:
            try:
                with os.scandir(path) as entries:
                    for entry in entries:
                        name = os.fsencode(entry.name)
                        child = name if relative == b"." else relative + b"/" + name
                        pending.append((Path(entry.path), child))
            except OSError:
                counts["read_directory_errors"] += 1
    result = {key: counts[key] for key in (
        "nodes", "files", "directories", "boundaries", "hidden_entries",
        "hardlinked_file_entries", "metadata_errors", "read_directory_errors",
    )}
    result["kinds"] = {kind: counts[f"kind_{kind}"]
                       for kind in ("directory", "file", "symlink", "other")}
    result["logical_bytes"] = logical_bytes
    result["expected_complete"] = not any(counts[key] for key in (
        "boundaries", "metadata_errors", "read_directory_errors"))
    result["inventory_digest"] = hashlib.sha256(b"".join(row for _, row in sorted(rows))).hexdigest()
    result["change_digest"] = hashlib.sha256(b"".join(row for _, row in sorted(changes))).hexdigest()
    return result


def validated_result(value: object) -> dict:
    """Accept only known scalar fields, preventing runner path data from leaking."""
    if not isinstance(value, dict) or REQUIRED_FIELDS - value.keys():
        raise BenchError("Runner JSON is missing required fields")
    result = {}
    for key in NUMERIC_FIELDS:
        number = value.get(key)
        if number is None and key not in REQUIRED_FIELDS:
            result[key] = None
            continue
        if isinstance(number, bool) or not isinstance(number, (int, float)):
            raise BenchError(f"Runner field {key} must be a nonnegative number")
        if number < 0 or not math.isfinite(number):
            raise BenchError(f"Runner field {key} must be finite and nonnegative")
        if key in INTEGER_FIELDS and not isinstance(number, int):
            raise BenchError(f"Runner field {key} must be an integer")
        result[key] = number
    for key in ("complete", "cancelled"):
        if not isinstance(value[key], bool):
            raise BenchError(f"Runner field {key} must be a boolean")
        result[key] = value[key]
    digest = value["validation_digest"]
    if not isinstance(digest, str) or re.fullmatch(r"[0-9a-fA-F]{64}", digest) is None:
        raise BenchError("Runner validation_digest must be a SHA-256 hex string")
    result["validation_digest"] = digest.lower()
    return result


def invoke(binary: Path, root: Path, scratch: Path, timeout: float,
           cancel_after_ms: int | None = None, enumeration: str = "default") -> dict:
    output = scratch / "runner-result.json"
    output.unlink(missing_ok=True)
    command = [str(binary), str(root), "--output", str(output)]
    if cancel_after_ms is not None:
        command.extend(("--cancel-after-ms", str(cancel_after_ms)))
    if enumeration == "portable":
        command.append("--portable-enumeration")
    started = time.perf_counter()
    try:
        # Discard output deliberately: it may contain private paths. The report
        # includes a fixed diagnostic label and exit code if the process fails.
        completed = subprocess.run(command, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                   timeout=timeout, check=False)
    except subprocess.TimeoutExpired:
        raise BenchError("Runner timed out") from None
    except OSError:
        raise BenchError("Runner could not be started") from None
    wall_ms = elapsed_ms(started)
    if completed.returncode:
        raise BenchError(f"Runner exited with status {completed.returncode}")
    try:
        with output.open("r", encoding="utf-8") as source:
            value = json.load(source)
    except (OSError, ValueError):
        raise BenchError("Runner did not write valid JSON") from None
    result = validated_result(value)
    result["process_wall_ms"] = wall_ms
    result["files_per_second"] = (result["files"] * 1000 / result["scan_ms"]
                                  if result["scan_ms"] else None)
    return result


def check_full_result(result: dict, expected: dict) -> list[str]:
    problems = []
    for key in ("nodes", "files", "directories", "logical_bytes"):
        if result[key] != expected[key]:
            problems.append(f"{key}: expected {expected[key]}, observed {result[key]}")
    if result["cancelled"]:
        problems.append("full scan unexpectedly cancelled")
    if result["complete"] != expected["expected_complete"]:
        problems.append("complete flag differs from independent inventory")
    return problems


def distribution(values: list[float | int]) -> dict | None:
    if not values:
        return None
    ordered = sorted(values)
    # Nearest-rank p95 is explicitly defined, including small sample sets.
    return {"samples": len(values), "median": statistics.median(values),
            "p95": ordered[math.ceil(len(ordered) * 0.95) - 1],
            "min": ordered[0], "max": ordered[-1]}


def summarize(runs: list[dict]) -> dict:
    return {key: distribution([run[key] for run in runs if run.get(key) is not None])
            for key in (*NUMERIC_FIELDS, "process_wall_ms", "files_per_second")}


def binary_info(binary: Path, role: str, profile_name: str, target: str | None,
                enumeration: str) -> dict:
    try:
        return {"role": role, "sha256": file_hash(binary), "bytes": binary.stat().st_size,
                "declared_profile": profile_name, "declared_target": target,
                "enumeration": enumeration}
    except OSError:
        raise BenchError(f"Cannot read {role} binary") from None


def environment_info() -> dict:
    rust = {}
    try:
        output = subprocess.run(["rustc", "-vV"], capture_output=True, text=True,
                                timeout=5, check=False)
        if output.returncode == 0:
            for line in output.stdout.splitlines():
                if line.startswith(("host: ", "release: ")):
                    key, value = line.split(": ", 1)
                    if re.fullmatch(r"[A-Za-z0-9_.+\-]+", value):
                        rust[key] = value
    except (OSError, subprocess.TimeoutExpired):
        pass
    return {"os": platform.system(), "os_release": platform.release(),
            "architecture": platform.machine(), "logical_cpus": os.cpu_count(),
            "python": platform.python_version(), "rustc": rust}


def benchmark_fixture(root: Path, descriptor: dict, binaries: list[tuple[str, Path]],
                      scratch: Path, args: argparse.Namespace) -> dict:
    started = time.perf_counter()
    before = inventory(root)
    audit_ms = elapsed_ms(started)
    change_digest = before.pop("change_digest")
    result = {"fixture": descriptor, "expected": before,
              "setup": {"create_ms": descriptor.pop("create_ms", 0.0),
                        "preflight_inventory_ms": audit_ms},
              "variants": {}, "validation_errors": []}
    expected_digest = None
    enumerations = {"candidate": args.enumeration, "reference": args.reference_enumeration}
    for role, _ in binaries:
        result["variants"][role] = {"warmups": [], "runs": [], "cancellation_runs": []}

    # Alternate binary order per iteration so reference/candidate don't always
    # receive the same cache or background-load position.
    for iteration in range(args.warmups + args.iterations):
        order = binaries if iteration % 2 == 0 else list(reversed(binaries))
        for role, binary in order:
            run = invoke(binary, root, scratch, args.timeout_seconds,
                         enumeration=enumerations[role])
            run["iteration"] = iteration if iteration < args.warmups else iteration - args.warmups
            run["validation_errors"] = check_full_result(run, before)
            if expected_digest is None:
                expected_digest = run["validation_digest"]
            elif run["validation_digest"] != expected_digest:
                run["validation_errors"].append("validation_digest differs across full scans")
            if run["validation_errors"]:
                result["validation_errors"].append(f"{role} full scan {iteration} failed validation")
            category = "warmups" if iteration < args.warmups else "runs"
            result["variants"][role][category].append(run)

    for iteration in range(args.cancel_iterations):
        order = binaries if iteration % 2 == 0 else list(reversed(binaries))
        for role, binary in order:
            run = invoke(binary, root, scratch, args.timeout_seconds, args.cancel_after_ms,
                         enumeration=enumerations[role])
            run["iteration"] = iteration
            run["requested_after_ms"] = args.cancel_after_ms
            if run["cancelled"]:
                run["outcome"] = "cancelled"
                errors = []
                if run["complete"]:
                    errors.append("cancelled scan reported complete")
                for key in ("nodes", "files", "directories", "logical_bytes"):
                    if run[key] > before[key]:
                        errors.append(f"cancelled scan {key} exceeds complete inventory")
                if run["nodes"] != run["files"] + run["directories"]:
                    errors.append("cancelled node count differs from files plus directories")
            else:
                run["outcome"] = ("completed_before_cancel" if run["scan_ms"] < args.cancel_after_ms
                                  else "completed_without_observing_cancel")
                errors = check_full_result(run, before)
                if run["validation_digest"] != expected_digest:
                    errors.append("completed cancellation run has a different digest")
            if errors:
                result["validation_errors"].append(f"{role} cancellation run failed validation")
            run["validation_errors"] = errors
            result["variants"][role]["cancellation_runs"].append(run)

    started = time.perf_counter()
    after = inventory(root)
    result["setup"]["postflight_inventory_ms"] = elapsed_ms(started)
    result["fixture_unchanged"] = change_digest == after.pop("change_digest")
    if not result["fixture_unchanged"]:
        result["validation_errors"].append("fixture changed during measurements")
    for variant in result["variants"].values():
        variant["summary"] = summarize(variant["runs"])
        cancellations = variant["cancellation_runs"]
        cancelled = [run for run in cancellations if run["cancelled"]]
        variant["cancellation_summary"] = {
            "attempts": len(cancellations), "cancelled": len(cancelled),
            "completed_before_cancel": sum(run["outcome"] == "completed_before_cancel" for run in cancellations),
            "completed_without_observing_cancel": sum(
                run["outcome"] == "completed_without_observing_cancel" for run in cancellations),
            "response_observed": bool(cancelled) if cancellations else None,
            "latency_ms": distribution([run["cancel_latency_ms"] for run in cancelled
                                        if run["cancel_latency_ms"] is not None]),
            "process_wall_ms": distribution([run["process_wall_ms"] for run in cancellations]),
        }
    if len(binaries) == 2:
        comparison = {}
        for key in ("scan_ms", "serialize_ms", "snapshot_bytes", "peak_rss_bytes", "metadata_calls"):
            candidate = result["variants"]["candidate"]["summary"][key]
            reference = result["variants"]["reference"]["summary"][key]
            comparison[f"{key}_candidate_over_reference"] = (
                candidate["median"] / reference["median"]
                if candidate and reference and reference["median"] else None)
        result["comparison"] = comparison
    result["validated"] = not result["validation_errors"]
    return result


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True, help="Core diagnostic runner")
    parser.add_argument("--reference-binary", type=Path, help="Compare a second compatible runner")
    parser.add_argument("--enumeration", choices=("default", "portable"), default="default",
                        help="Candidate enumeration mode; portable adds --portable-enumeration")
    parser.add_argument("--reference-enumeration", choices=("default", "portable"), default="default",
                        help="Reference enumeration mode; default adds no runner argument")
    selection = parser.add_mutually_exclusive_group()
    selection.add_argument("--root", type=Path, help="Explicit existing directory; read-only scans")
    selection.add_argument("--fixture", action="append", choices=(*FIXTURES, "all"),
                           help="Repeat to select fixtures; default: all")
    parser.add_argument("--small-files", type=int, default=100_000)
    parser.add_argument("--wide-files", type=int, default=50_000)
    parser.add_argument("--mixed-files", type=int, default=10_000)
    parser.add_argument("--deep-depth", type=int, default=128, help="1 to 128")
    parser.add_argument("--deep-files-per-level", type=int, default=1,
                        help="Files at each depth; default 1 preserves the original fixture")
    parser.add_argument("--seed", type=int, default=20261028)
    parser.add_argument("--iterations", type=int, default=5, help="Measured scans per binary")
    parser.add_argument("--warmups", type=int, default=1, help="Unmeasured scans per binary")
    parser.add_argument("--cancel-after-ms", type=int, default=20)
    parser.add_argument("--cancel-iterations", type=int, default=3, help="0 to disable cancellation runs")
    parser.add_argument("--timeout-seconds", type=float, default=300)
    parser.add_argument("--profile", choices=("release", "debug", "custom"), default="release",
                        help="Build profile declaration; script does not build or verify it")
    parser.add_argument("--reference-profile", choices=("release", "debug", "custom"), default="release")
    parser.add_argument("--target", help="Declared Rust target triple; defaults to local rustc host")
    parser.add_argument("--output", type=Path, help="Write JSON here (default: stdout)")
    args = parser.parse_args()
    if args.iterations < 1 or args.warmups < 0 or args.cancel_iterations < 0:
        parser.error("iterations must be positive; warmups/cancel-iterations must be nonnegative")
    if min(args.small_files, args.wide_files, args.mixed_files) < 0:
        parser.error("file counts must be nonnegative")
    if not 1 <= args.deep_depth <= 128:
        parser.error("deep-depth must be between 1 and 128")
    if args.deep_files_per_level < 1:
        parser.error("deep-files-per-level must be positive")
    if args.cancel_after_ms < 0 or not math.isfinite(args.timeout_seconds) or args.timeout_seconds <= 0:
        parser.error("cancel-after-ms must be nonnegative; timeout-seconds must be positive")
    if args.target and re.fullmatch(r"[A-Za-z0-9_.+\-]+", args.target) is None:
        parser.error("target must be a Rust target triple without paths")
    return args


def main() -> int:
    args = parse_args()
    try:
        started = time.perf_counter()
        environment = environment_info()
        target = args.target or environment["rustc"].get("host")
        binaries = [("candidate", args.binary.resolve())]
        info = [binary_info(binaries[0][1], "candidate", args.profile, target, args.enumeration)]
        if args.reference_binary:
            binaries.append(("reference", args.reference_binary.resolve()))
            info.append(binary_info(binaries[1][1], "reference", args.reference_profile, target,
                                    args.reference_enumeration))
        report = {
            "schema_version": 1,
            "created_at_utc": datetime.now(timezone.utc).isoformat(),
            "environment": environment, "binaries": info,
            "method": {"cache": "warm; no OS cache clearing", "iterations": args.iterations,
                       "warmups": args.warmups, "p95": "nearest rank",
                       "timing": "runner scan_ms excludes serialization; process_wall_ms includes startup and output",
                       "peak_rss": "OS process high-water mark read when scan returns, before diagnostic hashing/queries/serialization; bytes or null",
                       "binary_order": "alternates each iteration",
                       "enumeration": {item["role"]: item["enumeration"] for item in info},
                       "enumeration_arguments": "default: none; portable: --portable-enumeration",
                       "cancel_after_ms": args.cancel_after_ms,
                       "cancel_iterations": args.cancel_iterations,
                       "paths": "omitted; runner output restricted to known scalar fields"},
            "setup": {"environment_and_binary_inspection_ms": elapsed_ms(started)},
            "fixtures": [],
        }
        with ExitStack() as stack:
            temporary = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="ohmydisk-bench-")))
            scratch = temporary / "results"
            scratch.mkdir()
            if args.root:
                # resolve() is safe here after rejecting a final root link in
                # inventory(); do not silently reinterpret an explicitly linked root.
                root = args.root.absolute()
                canonical = root.resolve()
                if temporary.resolve().is_relative_to(canonical):
                    raise BenchError("Temporary runner output must be outside the selected root")
                if args.output and args.output.resolve().is_relative_to(canonical):
                    raise BenchError("Report output must be outside the read-only selected root")
                descriptor = {"kind": "existing-directory", "seed": None,
                              "parameters": {}, "features": {}, "create_ms": 0.0}
                selections = [(root, descriptor)]
            else:
                names = args.fixture or ["all"]
                selected = FIXTURES if "all" in names else tuple(dict.fromkeys(names))
                selections = []
                for name in selected:
                    root = temporary / name
                    root.mkdir()
                    created = time.perf_counter()
                    descriptor = make_fixture(root, name, args)
                    descriptor["create_ms"] = elapsed_ms(created)
                    selections.append((root, descriptor))
            for root, descriptor in selections:
                print(f"Benchmarking {descriptor['kind']}...", file=sys.stderr)
                report["fixtures"].append(benchmark_fixture(root, descriptor, binaries, scratch, args))
        report["validated"] = all(item["validated"] for item in report["fixtures"])
        serialized = json.dumps(report, indent=2, allow_nan=False) + "\n"
        if args.output:
            args.output.parent.mkdir(parents=True, exist_ok=True)
            args.output.write_text(serialized, encoding="utf-8")
        else:
            sys.stdout.write(serialized)
        return 0 if report["validated"] else 2
    except BenchError as error:
        print(f"Benchmark failed: {error}", file=sys.stderr)
        return 1
    except OSError as error:
        # str(error) usually includes the local path. Keep diagnostics private.
        print(f"Benchmark failed: filesystem operation ({error.__class__.__name__}, errno={error.errno})",
              file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
