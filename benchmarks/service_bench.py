#!/usr/bin/env python3
"""Measure service scans and first queries with isolated application state.

Python 3.11+, standard library only. Default: one excluded warmup and five
measured trials per binary, alternating binary order. No caches are cleared.
The selected root is only scanned; cleanup operations are never called.
"""

from __future__ import annotations

import argparse
from contextlib import contextmanager
from datetime import datetime, timezone
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
import uuid

try:
    from .scan_bench import inventory
except ImportError:
    from scan_bench import inventory


TERMINAL_STATES = {"completed", "completed_with_errors", "cancelled", "failed", "interrupted"}
HISTORICAL_METRICS = (
    "historical_cold_list_children_ms", "historical_cold_query_descendants_ms",
    "historical_warm_list_children_ms", "historical_warm_query_descendants_ms",
    "historical_children_restart_ms", "historical_descendants_restart_ms",
    "historical_children_sampled_max_rss_bytes", "historical_descendants_sampled_max_rss_bytes",
    "historical_children_rss_samples", "historical_descendants_rss_samples",
)
METRICS = (
    "start_scan_accept_ms", "scan_terminal_ms", "poll_rpc_total_ms", "poll_count",
    "get_scan_ms", "first_list_children_ms", "first_query_descendants_ms",
    "warm_list_children_ms", "warm_query_descendants_ms", "sampled_max_rss_bytes",
    "rss_samples", "state_database_bytes", "state_wal_bytes", "state_shm_bytes",
    "state_total_bytes", "snapshot_payload_bytes", "snapshot_stored_bytes",
    "snapshot_compression_ratio", "source_scan_ms", "source_persist_ms",
    "source_index_build_ms", "progress_writes", "files", "directories",
    "logical_bytes", "issue_count", "children_returned", "descendants_returned",
) + HISTORICAL_METRICS


class BenchError(Exception):
    """Only fixed, non-sensitive error codes may leave the runner."""


class PrivateArgumentParser(argparse.ArgumentParser):
    def error(self, message: str) -> None:
        raise BenchError("invalid_arguments")


def elapsed_ms(start: float) -> float:
    return (time.perf_counter() - start) * 1000


def positive_int(value: str) -> int:
    result = int(value)
    if result < 1:
        raise ValueError
    return result


def positive_float(value: str) -> float:
    result = float(value)
    if result <= 0 or not math.isfinite(result):
        raise ValueError
    return result


def within(path: Path, root: Path) -> bool:
    return path == root or root in path.parents


def hash_binary(path: Path) -> str:
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def lock_file(handle, *, blocking: bool) -> bool:
    """Match the OS advisory lock used by the application's fs2 service lock."""
    try:
        if os.name == "nt":
            import msvcrt

            handle.seek(0)
            msvcrt.locking(handle.fileno(), msvcrt.LK_LOCK if blocking else msvcrt.LK_NBLCK, 1)
        else:
            import fcntl

            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | (0 if blocking else fcntl.LOCK_NB))
        return True
    except (BlockingIOError, PermissionError):
        return False
    except OSError as error:
        # EACCES/EAGAIN on Unix, lock violation on Windows.
        if error.errno in (11, 13, 35, 36) or getattr(error, "winerror", None) in (32, 33):
            return False
        raise


def unlock_file(handle) -> None:
    if os.name == "nt":
        import msvcrt

        handle.seek(0)
        msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
    else:
        import fcntl

        fcntl.flock(handle.fileno(), fcntl.LOCK_UN)


def service_has_lock(state: Path) -> bool:
    if not (state / "state.sqlite").is_file():
        return False
    try:
        with (state / "service.lock").open("r+b") as handle:
            if lock_file(handle, blocking=False):
                unlock_file(handle)
                return False
            return True
    except FileNotFoundError:
        return False


@contextmanager
def prevent_implicit_spawn(state: Path):
    """Diagnostic calls must never auto-spawn an unowned background service.

    The foreground service does not need startup.lock. Holding it makes a
    diagnostic client's recovery loop time out if our service unexpectedly dies.
    """
    descriptor = os.open(state / "startup.lock", os.O_RDWR | os.O_CREAT | os.O_APPEND, 0o600)
    with os.fdopen(descriptor, "a+b") as handle:
        if not lock_file(handle, blocking=False):
            raise BenchError("startup_lock_unavailable")
        try:
            yield
        finally:
            unlock_file(handle)


def rpc(binary: Path, state: Path, service: subprocess.Popen, operation: str,
        arguments: dict, timeout: float) -> tuple[dict, float]:
    if service.poll() is not None:
        raise BenchError("service_stopped")
    command = [str(binary), "--data-dir", str(state), "call", operation,
               "--args", json.dumps(arguments, separators=(",", ":"))]
    started = time.perf_counter()
    try:
        completed = subprocess.run(command, stdin=subprocess.DEVNULL,
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   timeout=timeout, check=False)
    except subprocess.TimeoutExpired:
        raise BenchError("rpc_timeout") from None
    except OSError:
        raise BenchError("rpc_start_failed") from None
    duration = elapsed_ms(started)
    if completed.returncode != 0 or service.poll() is not None:
        raise BenchError("rpc_failed")
    try:
        value = json.loads(completed.stdout)
    except (ValueError, UnicodeError):
        raise BenchError("rpc_invalid_json") from None
    if not isinstance(value, dict):
        raise BenchError("rpc_invalid_result")
    return value, duration


def wait_ready(binary: Path, state: Path, service: subprocess.Popen,
               timeout: float) -> None:
    deadline = time.perf_counter() + timeout
    while not service_has_lock(state):
        if service.poll() is not None:
            raise BenchError("service_start_failed")
        if time.perf_counter() >= deadline:
            raise BenchError("service_start_timeout")
        time.sleep(0.02)
    # The lock and database precede listener creation. Probe only after ownership
    # is established, with startup.lock held to prevent accidental auto-spawning.
    while time.perf_counter() < deadline:
        try:
            value, _ = rpc(binary, state, service, "get_capabilities", {},
                           min(15.0, max(0.1, deadline - time.perf_counter())))
            if value.get("api_version") == 1:
                return
            raise BenchError("unsupported_service_api")
        except BenchError as error:
            if str(error) not in {"rpc_failed", "rpc_timeout"}:
                raise
            if service.poll() is not None:
                raise BenchError("service_start_failed") from None
            time.sleep(0.02)
    raise BenchError("service_ready_timeout")


class RssSampler:
    """Observed service RSS from ps; this is not a process peak measurement."""

    def __init__(self, service: subprocess.Popen, interval: float):
        self.service = service
        self.interval = interval
        self.maximum: int | None = None
        self.samples = 0
        self.stop_event = threading.Event()
        self.thread: threading.Thread | None = None

    def sample(self) -> None:
        if self.service.poll() is not None or os.name != "posix":
            return
        try:
            result = subprocess.run(["ps", "-o", "rss=", "-p", str(self.service.pid)],
                                    stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=subprocess.DEVNULL, timeout=2, check=False)
            if result.returncode == 0:
                rss = int(result.stdout.strip()) * 1024
                self.maximum = max(self.maximum or 0, rss)
                self.samples += 1
        except (OSError, ValueError, subprocess.TimeoutExpired):
            pass

    def run(self) -> None:
        while not self.stop_event.wait(self.interval):
            self.sample()

    def start(self) -> None:
        self.sample()
        self.thread = threading.Thread(target=self.run, daemon=True)
        self.thread.start()

    def stop(self) -> None:
        self.stop_event.set()
        if self.thread is not None:
            self.thread.join()
        self.sample()


def stop_service(service: subprocess.Popen) -> None:
    """Stop only the process created by this trial, and reap it before cleanup."""
    if service.poll() is None:
        service.terminate()
        try:
            service.wait(timeout=10)
        except subprocess.TimeoutExpired:
            service.kill()
            service.wait(timeout=10)
    else:
        service.wait()


def integer(value: object) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0:
        raise BenchError("invalid_numeric_result")
    return value


def optional_number(value: object) -> int | float | None:
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise BenchError("invalid_numeric_result")
    if value < 0 or not math.isfinite(value):
        raise BenchError("invalid_numeric_result")
    return value


def identifier(value: object) -> str:
    if not isinstance(value, str) or not value or len(value) > 128:
        raise BenchError("invalid_identifier_result")
    return value


def database_sizes(state: Path) -> dict:
    sizes = {}
    for label, name in (("state_database_bytes", "state.sqlite"),
                        ("state_wal_bytes", "state.sqlite-wal"),
                        ("state_shm_bytes", "state.sqlite-shm")):
        try:
            sizes[label] = (state / name).stat().st_size
        except FileNotFoundError:
            sizes[label] = 0
    sizes["state_total_bytes"] = sum(sizes.values())
    # Read only and outside scan/query timings. Legacy JSON text and compressed
    # BLOBs may coexist. Only aggregate lengths are read, never snapshot paths.
    import sqlite3

    connection = sqlite3.connect((state / "state.sqlite").as_uri() + "?mode=ro", uri=True)
    try:
        tables = {row[0] for row in connection.execute(
            "SELECT name FROM sqlite_master WHERE type='table' "
            "AND name IN ('entities','snapshot_blobs')")}
        original_bytes = 0
        stored_bytes = 0
        if "entities" in tables:
            row = connection.execute(
                "SELECT COALESCE(SUM(length(CAST(payload AS BLOB))),0) "
                "FROM entities WHERE kind='snapshot'").fetchone()
            legacy_bytes = integer(row[0])
            original_bytes += legacy_bytes
            stored_bytes += legacy_bytes
        if "snapshot_blobs" in tables:
            row = connection.execute(
                "SELECT COALESCE(SUM(json_bytes),0), "
                "COALESCE(SUM(length(payload)),0) FROM snapshot_blobs").fetchone()
            original_bytes += integer(row[0])
            stored_bytes += integer(row[1])
        sizes["snapshot_payload_bytes"] = original_bytes
        sizes["snapshot_stored_bytes"] = stored_bytes
        sizes["snapshot_compression_ratio"] = (stored_bytes / original_bytes
                                               if original_bytes else None)
    finally:
        connection.close()
    return sizes


def historical_query(binary: Path, state: Path, args: argparse.Namespace, *,
                     label: str, operation: str, query_args: dict,
                     expected_page: dict, expected_summary: dict) -> dict:
    """Load an existing snapshot in an owned process with an empty memory cache.

    The caller keeps startup.lock held across all service lifetimes. Readiness
    only probes capabilities; no snapshot request precedes the timed query.
    """
    service: subprocess.Popen | None = None
    sampler: RssSampler | None = None
    try:
        restarted = time.perf_counter()
        service = subprocess.Popen([str(binary), "--data-dir", str(state),
                                    "service", "--stay-alive"],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL)
        wait_ready(binary, state, service, args.startup_timeout)
        restart_ms = elapsed_ms(restarted)
        sampler = RssSampler(service, args.rss_interval)
        sampler.start()
        page, cold_ms = rpc(binary, state, service, operation, query_args, args.rpc_timeout)
        warm_page, warm_ms = rpc(binary, state, service, operation, query_args, args.rpc_timeout)
        if page != expected_page or warm_page != expected_page:
            raise BenchError("historical_query_results_changed")
        # Check persisted scan/snapshot identity and totals after the cold query,
        # so even a future implementation of get_scan cannot prewarm that query.
        restored_summary, _ = rpc(binary, state, service, "get_scan",
                                  {"scan_id": query_args["scan_id"]}, args.rpc_timeout)
        if restored_summary != expected_summary:
            raise BenchError("historical_scan_summary_changed")
        sampler.stop()
        query_metric = "list_children" if label == "children" else "query_descendants"
        return {
            f"historical_cold_{query_metric}_ms": cold_ms,
            f"historical_warm_{query_metric}_ms": warm_ms,
            f"historical_{label}_restart_ms": restart_ms,
            f"historical_{label}_sampled_max_rss_bytes": sampler.maximum,
            f"historical_{label}_rss_samples": sampler.samples,
        }
    finally:
        try:
            if sampler is not None and not sampler.stop_event.is_set():
                sampler.stop()
        finally:
            if service is not None:
                stop_service(service)


def trial(binary: Path, root: Path, state: Path, args: argparse.Namespace,
          expected: dict | None) -> dict:
    state.mkdir(mode=0o700)
    service: subprocess.Popen | None = None
    sampler: RssSampler | None = None
    with prevent_implicit_spawn(state):
        try:
            service = subprocess.Popen([str(binary), "--data-dir", str(state),
                                        "service", "--stay-alive"],
                                       stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                       stderr=subprocess.DEVNULL)
            wait_ready(binary, state, service, args.startup_timeout)
            sampler = RssSampler(service, args.rss_interval)
            sampler.start()
            started = time.perf_counter()
            accepted, accept_ms = rpc(binary, state, service, "start_scan",
                                      {"root_path": str(root), "request_key": str(uuid.uuid4())},
                                      args.rpc_timeout)
            scan_id = identifier(accepted.get("scan_id"))
            job_id = identifier(accepted.get("job_id"))
            poll_count = 0
            poll_rpc_total_ms = 0.0
            deadline = started + args.scan_timeout
            while True:
                if time.perf_counter() >= deadline:
                    raise BenchError("scan_timeout")
                job, poll_ms = rpc(binary, state, service, "get_job", {"job_id": job_id},
                                   min(args.rpc_timeout, max(0.1, deadline - time.perf_counter())))
                poll_count += 1
                poll_rpc_total_ms += poll_ms
                if job.get("state") in TERMINAL_STATES:
                    break
                time.sleep(min(args.poll_interval, max(0, deadline - time.perf_counter())))
            terminal_ms = elapsed_ms(started)
            if job.get("state") not in {"completed", "completed_with_errors"}:
                raise BenchError("scan_did_not_complete")
            progress = job.get("progress")
            if not isinstance(progress, dict):
                raise BenchError("scan_missing_progress")
            summary, summary_ms = rpc(binary, state, service, "get_scan", {"scan_id": scan_id},
                                      args.rpc_timeout)
            root_id = identifier(summary.get("root_id"))
            children_args = {"scan_id": scan_id, "node_id": root_id, "limit": 100}
            descendants_args = {"scan_id": scan_id, "scope_node_id": root_id,
                                "descendants": True, "limit": 500}
            children, first_children_ms = rpc(binary, state, service, "list_children",
                                              children_args, args.rpc_timeout)
            descendants, first_descendants_ms = rpc(binary, state, service, "query_nodes",
                                                    descendants_args, args.rpc_timeout)
            warm_children, warm_children_ms = rpc(binary, state, service, "list_children",
                                                  children_args, args.rpc_timeout)
            warm_descendants, warm_descendants_ms = rpc(binary, state, service, "query_nodes",
                                                        descendants_args, args.rpc_timeout)
            if children != warm_children or descendants != warm_descendants:
                raise BenchError("query_results_changed")
            for page in (children, descendants):
                if not isinstance(page.get("items"), list):
                    raise BenchError("query_missing_items")
            result = {
                "start_scan_accept_ms": accept_ms, "scan_terminal_ms": terminal_ms,
                "poll_rpc_total_ms": poll_rpc_total_ms, "poll_count": poll_count,
                "get_scan_ms": summary_ms, "first_list_children_ms": first_children_ms,
                "first_query_descendants_ms": first_descendants_ms,
                "warm_list_children_ms": warm_children_ms,
                "warm_query_descendants_ms": warm_descendants_ms,
                "files": integer(progress.get("files")),
                "directories": integer(progress.get("directories")),
                "logical_bytes": integer(summary.get("logical_bytes")),
                "issue_count": integer(summary.get("issue_count")),
                "complete": summary.get("complete") is True,
                "children_returned": len(children["items"]),
                "descendants_returned": len(descendants["items"]),
            }
            source = job.get("result")
            if not isinstance(source, dict):
                source = {}
            for metric, field in (("source_scan_ms", "scan_ms"),
                                  ("source_persist_ms", "persist_ms"),
                                  ("source_index_build_ms", "index_build_ms"),
                                  ("progress_writes", "progress_writes")):
                result[metric] = optional_number(source.get(field))
            if expected is not None:
                if any(result[key] != expected[key] for key in ("files", "directories", "logical_bytes")):
                    raise BenchError("fixture_scan_mismatch")
                if not result["complete"] or result["issue_count"]:
                    raise BenchError("fixture_scan_incomplete")
            sampler.stop()
            result["sampled_max_rss_bytes"] = sampler.maximum
            result["rss_samples"] = sampler.samples
            sampler = None
            result.update(database_sizes(state))
            result.update({key: None for key in HISTORICAL_METRICS})
            result["historical_results_verified"] = None
            if args.historical_restart:
                # Fresh-scan metrics and database sizes above retain their
                # original scope. Reap that process before reopening its state.
                stop_service(service)
                service = None
                for label, operation, query_args, expected_page in (
                    ("children", "list_children", children_args, children),
                    ("descendants", "query_nodes", descendants_args, descendants),
                ):
                    result.update(historical_query(
                        binary, state, args, label=label, operation=operation,
                        query_args=query_args, expected_page=expected_page,
                        expected_summary=summary))
                result["historical_results_verified"] = True
            return result
        finally:
            if sampler is not None:
                sampler.stop()
            if service is not None:
                stop_service(service)
    # State cleanup is performed by the caller after the startup lock is closed.


def make_fixture(root: Path, files: int) -> dict:
    root.mkdir()
    payload = bytes(range(128))
    for batch in range((files + 999) // 1000):
        directory = root / f"d{batch:06d}"
        directory.mkdir()
        for index in range(batch * 1000, min(files, (batch + 1) * 1000)):
            (directory / f"f{index:09d}").write_bytes(payload)
    return {"files": files, "directories": 1 + (files + 999) // 1000,
            "logical_bytes": files * len(payload)}


def summary(trials: list[dict]) -> dict:
    result = {}
    for key in METRICS:
        values = sorted(trial[key] for trial in trials if trial[key] is not None)
        result[key] = None if not values else {
            "median": statistics.median(values),
            "p95": values[max(0, math.ceil(len(values) * 0.95) - 1)],
        }
    return result


def parse_args() -> argparse.Namespace:
    parser = PrivateArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path,
                        help="Application binary with service and call subcommands")
    parser.add_argument("--reference-binary", type=Path,
                        help="Optional application binary measured in alternating order")
    parser.add_argument("--root", type=Path,
                        help="Explicit absolute, read-only root; otherwise create a fixture")
    parser.add_argument("--files", type=positive_int, default=100_000,
                        help="Generated fixture file count (default: 100000)")
    parser.add_argument("--runs", type=positive_int, default=5,
                        help="Measured runs per binary; one additional excluded warmup (default: 5)")
    parser.add_argument("--historical-restart", action="store_true",
                        help="Restart twice per trial to measure cold historical children and descendants")
    parser.add_argument("--poll-interval", type=positive_float, default=0.05,
                        help="Sleep after each nonterminal diagnostic poll, seconds (default: .05)")
    parser.add_argument("--rss-interval", type=positive_float, default=0.1,
                        help="Service ps RSS sample interval, seconds (default: .1)")
    parser.add_argument("--startup-timeout", type=positive_float, default=30)
    parser.add_argument("--rpc-timeout", type=positive_float, default=120)
    parser.add_argument("--scan-timeout", type=positive_float, default=3600)
    parser.add_argument("--scratch-dir", type=Path,
                        help="Existing temporary directory parent outside the selected root")
    parser.add_argument("--output", type=Path,
                        help="Optional aggregate JSON report outside the selected root")
    return parser.parse_args()


def scratch_parent(args: argparse.Namespace, root: Path | None) -> Path:
    candidates = ([args.scratch_dir] if args.scratch_dir else
                  [Path(tempfile.gettempdir()), Path("/tmp"), Path("/var/tmp"), Path.cwd()])
    for candidate in candidates:
        candidate = candidate.resolve()
        if candidate.is_dir() and (root is None or not within(candidate, root)):
            return candidate
    raise BenchError("scratch_directory_must_be_outside_root")


def public_inventory(value: dict) -> dict:
    return {key: value[key] for key in ("nodes", "files", "directories", "logical_bytes",
                                       "boundaries", "metadata_errors", "read_directory_errors",
                                       "inventory_digest")}


def run(args: argparse.Namespace) -> dict:
    binaries = {"candidate": args.binary.resolve()}
    if args.reference_binary is not None:
        binaries["reference"] = args.reference_binary.resolve()
    hashes = {label: hash_binary(path) for label, path in binaries.items()}
    root = args.root
    before = None
    if root is not None:
        if not root.is_absolute() or root.is_symlink():
            raise BenchError("root_must_be_absolute_real_directory")
        root = root.resolve()
        if args.output is not None and within(args.output.resolve(), root):
            raise BenchError("report_must_be_outside_root")
    parent = scratch_parent(args, root)
    if root is not None:
        before = inventory(root)
    measured = {label: [] for label in binaries}
    order = []
    # Only this generated workspace is removed. The explicit scan root is never
    # passed to rmtree, unlink, trash, or any application cleanup operation.
    with tempfile.TemporaryDirectory(prefix="ohmydisk-service-bench-", dir=parent) as temporary:
        scratch = Path(temporary).resolve()
        expected = None
        if root is None:
            root = scratch / "fixture"
            expected = make_fixture(root, args.files)
        elif within(scratch, root):
            raise BenchError("scratch_directory_must_be_outside_root")
        labels = list(binaries)
        try:
            for index in range(args.runs + 1):
                current_order = labels if index % 2 == 0 else list(reversed(labels))
                for label in current_order:
                    state = scratch / f"state-{index:03d}-{label}"
                    try:
                        result = trial(binaries[label], root, state, args, expected)
                    finally:
                        # trial() has reaped its child and closed all handles.
                        if state.exists():
                            shutil.rmtree(state)
                    if index:
                        result["trial"] = index
                        measured[label].append(result)
                        order.append({"trial": index, "binary_sha256": hashes[label]})
        finally:
            if before is not None:
                after = inventory(root)
                if before != after:
                    raise BenchError("selected_root_changed_during_benchmark")
        source = ({"kind": "generated_small_files", "expected": expected}
                  if before is None else
                  {"kind": "explicit_read_only_root", "inventory_verified_unchanged": True,
                   "inventory": public_inventory(before)})
    if any(hash_binary(path) != hashes[label] for label, path in binaries.items()):
        raise BenchError("binary_changed_during_benchmark")
    report = {
        "schema_version": 1,
        "created_at_utc": datetime.now(timezone.utc).isoformat(),
        "environment": {"system": platform.system(), "release": platform.release(),
                        "machine": platform.machine(), "python": platform.python_version()},
        "configuration": {"warmup_runs_per_binary": 1, "measured_runs_per_binary": args.runs,
                          "poll_interval_ms": args.poll_interval * 1000,
                          "rss_sample_interval_ms": args.rss_interval * 1000,
                          "children_limit": 100, "descendants_limit": 500,
                          "historical_restart": args.historical_restart},
        "measurement_notes": [
            "Warm filesystem cache; operating-system caches are not cleared.",
            "Each trial starts an owned service with fresh state outside the scan root.",
            "Service startup, fixture generation, inventory and cleanup are outside timings.",
            "RPC timings include diagnostic CLI process startup, IPC and JSON parsing.",
            "Scan terminal timing starts before start_scan and ends when a terminal poll returns.",
            "Polling CLI process time and nonterminal poll sleeps are included in scan terminal timing.",
            "First children, first descendants, warm children and warm descendants run sequentially.",
            "Sampled maximum RSS is an observed ps sample, not the service process peak; null if unavailable.",
            "Database sizes include SQLite, WAL and SHM while the service remains alive; no checkpoint is forced.",
            "Snapshot original JSON and stored payload bytes sum legacy entities and snapshot_blobs outside timings.",
            "Optional source timings and progress writes come from terminal job.result; null for older services.",
            "Optional historical queries each use a separate owned service restart with the same state and scan, without rescanning.",
            "Historical cold means an empty service memory cache, not cold filesystem or OS caches; readiness only probes capabilities.",
            "Historical warm timings repeat the same first-page query; results and persisted scan summary must equal the fresh-scan originals.",
            "Historical restart readiness and sampled RSS are separate; original fresh-scan and state size metrics end before any restart.",
            "No GUI startup, chart layout, drawing or 650ms GUI polling delay is measured.",
        ],
        "source": source, "trial_order": order,
        "binaries": {label: {"sha256": hashes[label], "trials": measured[label],
                              "summary": summary(measured[label])} for label in binaries},
    }
    if "reference" in binaries:
        ratios = {}
        candidate = report["binaries"]["candidate"]["summary"]
        reference = report["binaries"]["reference"]["summary"]
        for key in METRICS:
            left, right = candidate[key], reference[key]
            ratios[key] = (left["median"] / right["median"]
                           if left is not None and right is not None and right["median"] else None)
        report["candidate_over_reference_median_ratio"] = ratios
    return report


def main() -> int:
    try:
        args = parse_args()
        report = run(args)
        encoded = json.dumps(report, indent=2, sort_keys=True, allow_nan=False) + "\n"
        if args.output is not None:
            args.output.write_text(encoded, encoding="utf-8")
        else:
            sys.stdout.write(encoded)
        return 0
    except BenchError as error:
        sys.stderr.write(json.dumps({"error": str(error)}) + "\n")
    except KeyboardInterrupt:
        sys.stderr.write('{"error":"benchmark_interrupted"}\n')
    except Exception:
        # OS, sqlite, JSON and inventory errors may embed private paths. Never
        # print an exception, a command line, binary output or a traceback.
        sys.stderr.write('{"error":"service_benchmark_failed"}\n')
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
