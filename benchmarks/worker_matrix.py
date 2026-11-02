#!/usr/bin/env python3
"""Balanced 4/8/CPU scan experiment using owned, generated fixtures only.

Build the release diagnostic runner first; this program never builds, clears
caches, or accepts an existing user directory. See --help for the reproducible
command. Only scalar results, relative source names, and hashes enter reports.
"""
from __future__ import annotations

import argparse
from datetime import datetime, timezone
import hashlib
import itertools
import json
import math
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time

import scan_bench as bench


def measured_orders(labels: tuple[str, ...], count: int, offset: int = 0) -> list[tuple[str, ...]]:
    """Every permutation per block balances positions and directed neighbours."""
    orders = list(itertools.permutations(labels))
    if count < len(orders) or count % len(orders):
        raise ValueError("iterations must be a positive multiple of all variant permutations")
    return [orders[(index + offset) % len(orders)] for index in range(count)]


def warmup_orders(labels: tuple[str, ...], count: int) -> list[tuple[str, ...]]:
    return [labels[index % len(labels):] + labels[:index % len(labels)] for index in range(count)]


def hardware(binary: Path) -> dict:
    try:
        result = subprocess.run([str(binary), "--hardware-info"], capture_output=True,
                                text=True, timeout=10, check=True)
        native = json.loads(result.stdout)
        detected = native["available_parallelism"]
        if isinstance(detected, bool) or not isinstance(detected, int) or detected < 1:
            raise ValueError
    except (OSError, subprocess.SubprocessError, ValueError, KeyError, TypeError):
        raise bench.BenchError("Runner did not report available parallelism") from None
    result = {**bench.environment_info(), "available_parallelism": detected,
              "cpu_workers_effective": min(detected, 64),
              "cpu_workers_clamped": detected > 64,
              "runtime_worker_threads": native.get("runtime_worker_threads")}
    if sys.platform == "darwin":
        for key, sysctl in (("cpu", "machdep.cpu.brand_string"),
                            ("physical_cpus", "hw.physicalcpu"),
                            ("memory_bytes", "hw.memsize"), ("os_build", "kern.osversion")):
            try:
                value = subprocess.run(["sysctl", "-n", sysctl], capture_output=True,
                                       text=True, timeout=5, check=True).stdout.strip()
                result[key] = int(value) if key in ("physical_cpus", "memory_bytes") else value
            except (OSError, subprocess.SubprocessError, ValueError):
                result[key] = None
    return result


def load_snapshot() -> dict:
    result = {"captured_at_utc": datetime.now(timezone.utc).isoformat()}
    try:
        result["system_load_average_1_5_15m"] = list(os.getloadavg())
    except (AttributeError, OSError):
        result["system_load_average_1_5_15m"] = None
    # Aggregate only: no process names, PIDs, paths, or command lines are retained.
    if os.name == "posix":
        try:
            values = subprocess.run(["ps", "-A", "-o", "%cpu="], capture_output=True,
                                    text=True, timeout=5, check=True).stdout.split()
            result["sum_process_cpu_percent_ps_snapshot"] = sum(map(float, values))
            result["cpu_snapshot_note"] = "ps CPU percentages have OS-specific averaging; 100% means one core"
        except (OSError, subprocess.SubprocessError, ValueError):
            result["sum_process_cpu_percent_ps_snapshot"] = None
    return result


def source_info(repository: Path) -> dict:
    result = {}
    for key, command in (("source_commit", ["git", "rev-parse", "HEAD"]),
                         ("working_tree_diff_sha256", ["git", "diff", "--binary", "HEAD"])):
        try:
            output = subprocess.run(command, cwd=repository, capture_output=True,
                                    timeout=10, check=True).stdout
            result[key] = output.decode().strip() if key == "source_commit" else hashlib.sha256(output).hexdigest()
        except (OSError, subprocess.SubprocessError, UnicodeError):
            result[key] = None
    return result


def validate_run(run: dict, expected: dict, workers: int, fingerprints: dict) -> list[str]:
    errors = bench.check_full_result(run, expected)
    if run["scan_mode"] != "async" or run["scan_workers"] != workers:
        errors.append("runner did not use the selected async worker count")
    if run["filesystem_permits"] != workers:
        errors.append("runner filesystem budget differs from selected workers")
    for field in ("validation_digest",):
        if not run.get(field):
            errors.append(f"runner did not provide {field}")
        elif field not in fingerprints:
            fingerprints[field] = run[field]
        elif run[field] != fingerprints[field]:
            errors.append(f"{field} differs across worker configurations")
    return errors


def run_fixture(root: Path, descriptor: dict, binary: Path, scratch: Path,
                workers: dict[str, int], args: argparse.Namespace, offset: int) -> dict:
    started = time.perf_counter()
    expected = bench.inventory(root)
    change_digest = expected.pop("change_digest")
    result = {"fixture": descriptor, "expected": expected,
              "inventory_ms": bench.elapsed_ms(started), "load_before": load_snapshot(),
              "variants": {label: {"workers": count, "warmups": [], "runs": []}
                           for label, count in workers.items()},
              "trial_order": [], "validation_errors": [], "occupancy_fingerprints": {}}
    fingerprints = {}
    labels = tuple(workers)
    for phase, orders in (("warmups", warmup_orders(labels, args.warmups)),
                          ("runs", measured_orders(labels, args.iterations, offset))):
        for iteration, order in enumerate(orders):
            result["trial_order"].append({"phase": phase, "iteration": iteration, "order": order})
            for label in order:
                run = bench.invoke(binary, root, scratch, args.timeout_seconds, workers=workers[label])
                run["iteration"] = iteration
                errors = validate_run(run, expected, workers[label], fingerprints)
                occupancy = run.get("occupancy_digest")
                if occupancy:
                    result["occupancy_fingerprints"][occupancy] = result["occupancy_fingerprints"].get(occupancy, 0) + 1
                run["validation_errors"] = errors
                result["variants"][label][phase].append(run)
                if errors:
                    result["validation_errors"].append(f"{label} {phase} {iteration}: " + "; ".join(errors))
                    raise bench.BenchError("A matrix scan failed correctness or configuration validation")
            medians = ", ".join(f"{label}={result['variants'][label][phase][-1]['scan_ms']:.1f}ms"
                                for label in labels)
            print(f"{descriptor['case']} {phase} {iteration + 1}/{len(orders)}: {medians}",
                  file=sys.stderr, flush=True)
    started = time.perf_counter()
    after = bench.inventory(root)
    result["postflight_inventory_ms"] = bench.elapsed_ms(started)
    result["fixture_unchanged"] = change_digest == after.pop("change_digest") and expected == after
    if not result["fixture_unchanged"]:
        result["validation_errors"].append("fixture changed during measurements")
    for variant in result["variants"].values():
        variant["summary"] = bench.summarize(variant["runs"])
        variant["mean_cpu_cores_during_scan"] = bench.distribution([
            run["scan_cpu_ms"] / run["scan_ms"] for run in variant["runs"]
            if run.get("scan_cpu_ms") is not None and run["scan_ms"] > 0])
    result["paired_ratios"] = {}
    baseline = result["variants"]["workers-4"]["runs"]
    for label, variant in result["variants"].items():
        result["paired_ratios"][label + "_over_workers-4"] = bench.distribution([
            run["scan_ms"] / base["scan_ms"] for run, base in zip(variant["runs"], baseline)])
    result["fingerprints"] = fingerprints
    result["occupancy_stable"] = len(result["occupancy_fingerprints"]) == 1
    result["occupancy_note"] = "Private-size changes may occur with APFS allocation or external snapshots without changing files; occupancy stability is reported separately, not a structural correctness gate"
    result["load_after"] = load_snapshot()
    result["validated"] = not result["validation_errors"]
    return result


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--iterations", type=int, default=12,
                        help="Measured trials per configuration; positive multiple of 6, default 12")
    parser.add_argument("--warmups", type=int, default=3)
    parser.add_argument("--timeout-seconds", type=float, default=300)
    parser.add_argument("--include-large-directory", action="store_true",
                        help="Also measure 20k populated + 20k empty directories")
    parser.add_argument("--seed", type=int, default=20261102)
    args = parser.parse_args()
    if args.iterations < 6 or args.iterations % 6:
        parser.error("iterations must be a positive multiple of 6")
    if args.warmups < 0 or args.warmups % 3:
        parser.error("warmups must be a nonnegative multiple of 3")
    if not math.isfinite(args.timeout_seconds) or args.timeout_seconds <= 0:
        parser.error("timeout-seconds must be finite and positive")
    return args


def main() -> int:
    args = parse_args()
    binary = args.binary.resolve()
    try:
        environment = hardware(binary)
        workers = {"workers-4": 4, "workers-8": 8, "workers-cpu": environment["cpu_workers_effective"]}
        report = {"schema_version": 1, "created_at_utc": datetime.now(timezone.utc).isoformat(),
                  "environment": environment, **source_info(Path(__file__).resolve().parents[1]),
                  "binary_sha256": bench.file_hash(binary), "binary_bytes": binary.stat().st_size,
                  "tool_sha256": {name: bench.file_hash(Path(__file__).with_name(name))
                                  for name in ("scan_bench.py", "worker_matrix.py")},
                  "method": {"cache": "warm; no cache clearing; independent inventory warms metadata",
                             "iterations_per_variant": args.iterations, "warmups_per_variant": args.warmups,
                             "measured_order": "all six permutations twice at default 12 trials; balanced positions and within-trial directed neighbours",
                             "warmup_order": "cyclic rotations, balanced positions at default 3 warmups",
                             "p95": "nearest rank; with 12 trials this is maximum, not a stable tail estimate",
                             "workers": workers, "filesystem_budget": "per isolated scan equals selected workers; this experiment is not the service shared budget",
                             "runtime_worker_threads": 2, "batch_entries": 128, "batch_bytes": 262144,
                             "timing": "scan_ms includes root preparation/native observations/traversal/worker joining/reduction; excludes runtime setup, serialization, digest, index and queries",
                             "rss": "OS process high-water at scan return, before diagnostic serialization/hash/index/query work",
                             "cpu": "process user+system CPU time delta across scan, all process threads; null if unsupported",
                             "correctness": "every run checked against independent preflight inventory and matching logical+allocated fingerprint; postflight confirms unchanged fixture; per-node occupancy fingerprints report stability separately because APFS allocation/snapshots can change independently",
                             "scope": "owned synthetic fixtures only; warm-cache single scan; not real-home, cold disk, simultaneous service load, or all devices",
                             "paths": "fixture and process paths omitted"}, "fixtures": []}
        fixture_args = argparse.Namespace(seed=args.seed, small_files=100000, wide_files=50000,
                                         mixed_files=10000, deep_depth=128, deep_files_per_level=64,
                                         directory_count=5000, directory_files_per_dir=2,
                                         empty_directories=5000)
        cases = [(name, name, fixture_args) for name in (*bench.FIXTURES, "directory-heavy")]
        if args.include_large_directory:
            large = argparse.Namespace(**vars(fixture_args))
            large.directory_count = large.empty_directories = 20000
            cases.append(("directory-heavy-large", "directory-heavy", large))
        args.output.parent.mkdir(parents=True, exist_ok=True)
        def checkpoint() -> None:
            args.output.write_text(json.dumps(report, indent=2, allow_nan=False) + "\n", encoding="utf-8")
        checkpoint()
        with tempfile.TemporaryDirectory(prefix="omd-worker-matrix-") as temporary:
            owned = Path(temporary)
            scratch = owned / "results"
            scratch.mkdir()
            for offset, (case, kind, fixture_options) in enumerate(cases):
                root = owned / case
                root.mkdir()
                started = time.perf_counter()
                descriptor = bench.make_fixture(root, kind, fixture_options)
                descriptor.update(case=case, create_ms=bench.elapsed_ms(started))
                print(f"Fixture {case} created; beginning balanced scans", file=sys.stderr, flush=True)
                report["fixtures"].append(run_fixture(root, descriptor, binary, scratch, workers, args, offset))
                checkpoint()
        report["fixtures_cleaned"] = True
        report["binary_unchanged"] = report["binary_sha256"] == bench.file_hash(binary)
        report["validated"] = report["binary_unchanged"] and all(item["validated"] for item in report["fixtures"])
        report["completed_at_utc"] = datetime.now(timezone.utc).isoformat()
        checkpoint()
        return 0 if report["validated"] else 2
    except (bench.BenchError, OSError) as error:
        # BenchError labels never contain private fixture paths; OSError details can.
        print(f"Matrix failed: {error if isinstance(error, bench.BenchError) else 'filesystem operation failed'}",
              file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
