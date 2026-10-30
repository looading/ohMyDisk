#!/usr/bin/env python3
"""Exercise the real stdio adapter and shared backend using only temporary files."""
import argparse
from contextlib import contextmanager
import json
import os
from pathlib import Path
import queue
import subprocess
import sys
import tempfile
import threading
import time

REPO = Path(__file__).resolve().parents[1]
EXT = ".exe" if os.name == "nt" else ""
TERMINAL = {"completed", "completed_with_errors", "cancelled", "failed", "interrupted"}


def default_binary():
    configured = os.environ.get("OHMYDISK_BIN")
    if configured:
        return Path(configured).resolve()
    candidates = [REPO / "target" / profile / ("ohmydisk" + EXT) for profile in ("release", "debug")]
    return next((p for p in candidates if p.is_file()), candidates[0])


class Adapter:
    def __init__(self, binary, state):
        self.process = subprocess.Popen([str(binary), "mcp", "--stdio", "--data-dir", str(state)], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, encoding="utf-8", bufsize=1)
        self.messages = queue.Queue()
        self.stderr = []
        self.counter = 0
        def reader():
            for line in self.process.stdout:
                try:
                    self.messages.put(json.loads(line))
                except json.JSONDecodeError as error:
                    self.messages.put(AssertionError(f"stdout is not JSON-RPC: {line!r}: {error}"))
            self.messages.put(EOFError("MCP adapter closed stdout"))
        def diagnostic_reader():
            self.stderr.extend(self.process.stderr.readlines())
        self.readers = [threading.Thread(target=reader, daemon=True), threading.Thread(target=diagnostic_reader, daemon=True)]
        for thread in self.readers:
            thread.start()
        try:
            initialized = self.request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "omd-fixture-smoke", "version": "0.1.0"}})
            assert initialized["protocolVersion"] == "2025-11-25", initialized
            assert initialized["serverInfo"]["name"] == "omd", initialized
            self.server_version = initialized["serverInfo"]["version"]
            self.notify("notifications/initialized")
        except BaseException:
            self.stop()
            raise

    def notify(self, method, params=None):
        message = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            message["params"] = params
        self.process.stdin.write(json.dumps(message, ensure_ascii=False) + "\n")
        self.process.stdin.flush()

    def request(self, method, params):
        self.counter += 1
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.counter, "method": method, "params": params}, ensure_ascii=False) + "\n")
        self.process.stdin.flush()
        deadline = time.monotonic() + 30
        while True:
            message = self.messages.get(timeout=max(0.1, deadline - time.monotonic()))
            if isinstance(message, BaseException):
                raise AssertionError(f"{message}; stderr={''.join(self.stderr)}")
            assert message.get("jsonrpc") == "2.0", message
            if "method" in message:
                continue
            assert message.get("id") == self.counter, message
            assert "error" not in message, message
            return message["result"]

    def call(self, operation, arguments=None, error=None):
        result = self.request("tools/call", {"name": "omd_" + operation, "arguments": arguments or {}})
        value = result.get("structuredContent")
        assert isinstance(value, dict), result
        assert json.loads(result["content"][0]["text"]) == value, result
        if error is not None:
            assert result.get("isError") is True, result
            assert value["error"]["code"] == error, value
        else:
            assert not result.get("isError"), value
        return value

    def wait(self, accepted):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            job = self.call("get_job", {"job_id": accepted["job_id"]})
            if job["state"] in TERMINAL:
                assert job["state"] == "completed", job
                return job
            time.sleep(0.01)
        raise AssertionError(f"Background job timed out: {accepted}")

    def stop(self):
        if not self.process.stdin.closed:
            try:
                self.process.stdin.close()
            except BrokenPipeError:
                pass
        try:
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=5)
            raise AssertionError("MCP adapter did not exit after stdin EOF")
        finally:
            for thread in self.readers:
                thread.join(timeout=1)
            self.process.stdout.close()
            self.process.stderr.close()

    def close(self):
        self.stop()
        assert self.process.returncode == 0, (self.process.returncode, self.stderr)
        assert not self.stderr, self.stderr


@contextmanager
def temporary_fixture():
    temporary = tempfile.TemporaryDirectory(prefix="ohmydisk-mcp-")
    try:
        yield Path(temporary.name).resolve()
    finally:
        # An auto-started Windows backend owns its SQLite handles until normal idle exit.
        # Retry only this fixture's cleanup; never terminate unrelated processes.
        deadline = time.monotonic() + 90
        while True:
            try:
                temporary.cleanup()
                break
            except PermissionError:
                if os.name != "nt" or time.monotonic() >= deadline:
                    raise
                time.sleep(0.25)


class FixtureService:
    def __init__(self, binary, state, mode):
        self.binary, self.state, self.mode = binary, state, mode
        self.adapters = []
        self.process = None
        self.stderr = tempfile.TemporaryFile(mode="w+b")

    def __enter__(self):
        try:
            if self.mode == "supervised":
                self.process = subprocess.Popen(
                    [str(self.binary), "service", "--stay-alive", "--data-dir", str(self.state)],
                    stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=self.stderr,
                )
                # Engine opens SQLite only after this process owns service.lock.
                # Wait for ownership before a probing client can auto-spawn a rival.
                deadline = time.monotonic() + 20
                while not (self.state / "state.sqlite").exists():
                    assert self.process.poll() is None, "Supervised fixture service exited before acquiring its state"
                    assert time.monotonic() < deadline, "Supervised fixture service did not open its state"
                    time.sleep(0.01)
                # A real client probe waits for IPC readiness without assuming startup timing.
                ready = subprocess.run(
                    [str(self.binary), "call", "get_capabilities", "--data-dir", str(self.state)],
                    capture_output=True, text=True, encoding="utf-8", timeout=20,
                )
                assert ready.returncode == 0, ready.stderr
                assert self.process.poll() is None, "Supervised fixture service exited during startup"
            return self
        except BaseException:
            self.__exit__(*sys.exc_info())
            raise

    def adapter(self):
        if self.process is not None:
            assert self.process.poll() is None, "Supervised fixture service exited unexpectedly"
        adapter = Adapter(self.binary, self.state)
        self.adapters.append(adapter)
        return adapter

    def diagnostics(self):
        self.stderr.seek(0)
        captured = self.stderr.read().decode("utf-8", errors="replace")
        log_path = self.state / "service.log"
        log = log_path.read_text(encoding="utf-8", errors="replace") if log_path.exists() else ""
        print(f"Fixture service mode={self.mode}; captured stderr:\n{captured}\nservice.log:\n{log}", file=sys.stderr)

    def __exit__(self, error_type, error, traceback):
        cleanup_error = None
        for adapter in reversed(self.adapters):
            try:
                adapter.stop()
            except BaseException as failure:
                cleanup_error = cleanup_error or failure
        try:
            if self.process is not None:
                if self.process.poll() is None:
                    self.process.terminate()
                    try:
                        self.process.wait(timeout=15)
                    except subprocess.TimeoutExpired:
                        self.process.kill()
                        self.process.wait(timeout=5)
                        raise AssertionError("Supervised fixture service did not stop")
                elif error is None:
                    raise AssertionError(f"Supervised fixture service exited unexpectedly: {self.process.returncode}")
        except BaseException as failure:
            cleanup_error = cleanup_error or failure
        try:
            if error is not None or cleanup_error is not None:
                self.diagnostics()
        finally:
            self.stderr.close()
        if cleanup_error is not None and error is None:
            raise cleanup_error
        return False


def attributed_value(node):
    usage = node.get("space_usage")
    assert isinstance(usage, dict), node
    for key in ("attributed_bytes", "measured_entries", "unmeasured_entries"):
        assert isinstance(usage[key], int) and not isinstance(usage[key], bool) and usage[key] >= 0, usage
    if usage["measured_entries"] == 0 and usage["unmeasured_entries"] > 0:
        return None
    return usage["attributed_bytes"]


def assert_attributed_page(page):
    assert page["size_metric"] == "attributed", page
    def order(node):
        value = attributed_value(node)
        return value is None, -value if value is not None else 0, node["id"]
    assert page["items"] == sorted(page["items"], key=order), page


def assert_space_accounting(summary, file_nodes):
    accounting = summary.get("space_accounting")
    if accounting is None:
        # Native sampling may fail or be unsupported. Raw metadata remains
        # browsable, but an absent observation is not a measured zero ledger.
        assert all(attributed_value(node) is None for node in file_nodes), file_nodes
        return {"provider": "unsupported", "measurement_status": "unavailable", "reconciliation_status": "unavailable", "ledger_verified": False}
    assert accounting["measurement_status"] in {"complete", "partial", "unavailable"}, accounting
    assert accounting["reconciliation_status"] in {"balanced", "inconsistent", "unavailable"}, accounting
    provider = accounting["before"]["provider"]
    assert provider in {"mac_private_size", "ntfs_unshared_allocation", "unsupported"}, accounting
    measured = [node for node in file_nodes if attributed_value(node) is not None]
    unmeasured = [node for node in file_nodes if attributed_value(node) is None]
    assert accounting["attributed_bytes"] == sum(attributed_value(node) for node in measured), accounting
    if provider == "unsupported":
        assert not measured and unmeasured, accounting
        assert accounting["measurement_status"] == "unavailable", accounting
    if unmeasured:
        assert accounting["measurement_status"] != "complete", accounting
    after = accounting.get("after")
    if after is not None and after["capacity_scope"] == "caller_view":
        assert accounting["reconciliation_status"] != "balanced", accounting
    verified = accounting["reconciliation_status"] == "balanced"
    if verified:
        assert after is not None and after["capacity_scope"] != "caller_view", accounting
        assert after["volume_id"] == accounting["before"]["volume_id"], accounting
        assert after["total_bytes"] == accounting["before"]["total_bytes"], accounting
        capacity, available = after["total_bytes"], after["available_bytes"]
        attributed, remainder = accounting["attributed_bytes"], accounting["shared_unattributed_bytes"]
        assert all(isinstance(value, int) and value >= 0 for value in (capacity, available, attributed, remainder)), accounting
        assert capacity == available + attributed + remainder, accounting
    else:
        assert accounting["shared_unattributed_bytes"] is None, accounting
    return {"provider": provider, "measurement_status": accounting["measurement_status"], "reconciliation_status": accounting["reconciliation_status"], "ledger_verified": verified}


def run(binary, test_trash, service_mode="auto"):
    started = time.monotonic()
    with temporary_fixture() as fixture:
        root = fixture / "files"
        root.mkdir()
        (root / "keep.txt").write_text("keep", encoding="utf-8")
        (root / ".hidden").write_bytes(b"hidden")
        remove = root / "permanent target.txt"
        remove.write_bytes(b"temporary deletion fixture")
        nested = root / "nested"
        nested.mkdir()
        (nested / "child.txt").write_bytes(b"nested")
        try:
            os.link(nested / "child.txt", nested / "child-hardlink.txt")
            hardlink_created = True
        except OSError:
            # Some temporary filesystems do not implement hard links.
            hardlink_created = False
        state = fixture / "state"
        with FixtureService(binary, state, service_mode) as session:
            adapter = session.adapter()
            tools = adapter.request("tools/list", {})["tools"]
            assert len(tools) == 17, [t["name"] for t in tools]
            assert all(tool["name"].startswith("omd_") for tool in tools), tools
            definitions = {tool["name"]: tool for tool in tools}
            for name in ("omd_execute_trash", "omd_execute_permanent"):
                assert definitions[name]["annotations"]["destructiveHint"] is True
                assert definitions[name]["annotations"]["readOnlyHint"] is False
            capabilities = adapter.call("get_capabilities")
            assert capabilities["authorization_model"] == "host_confirmed"
            assert capabilities["application_version"] == adapter.server_version
            assert set(capabilities["size_metrics"]) == {"logical", "allocated", "attributed"}, capabilities
            assert capabilities["default_display_metric"] == "attributed", capabilities
            assert capabilities["legacy_query_default"] == "logical", capabilities
            for name in ("omd_list_children", "omd_query_nodes"):
                assert set(definitions[name]["inputSchema"]["properties"]["size_metric"]["enum"]) == {"logical", "allocated", "attributed"}
            assert isinstance(adapter.call("list_sources")["items"], list)
            scan_input = {"root_path": str(root), "request_key": "smoke-scan"}
            accepted = adapter.call("start_scan", scan_input)
            assert adapter.call("start_scan", scan_input) == accepted
            adapter.call("start_scan", {"root_path": str(nested), "request_key": "smoke-scan"}, error="request_conflict")
            # Closing this adapter must not cancel the accepted backend scan.
            adapter.close()
            adapter = session.adapter()
            assert adapter.call("start_scan", scan_input) == accepted
            adapter.wait(accepted)
            scan_id = accepted["scan_id"]
            summary = adapter.call("get_scan", {"scan_id": scan_id})
            assert summary["complete"] is True and "nodes" not in summary, summary
            assert summary["default_display_metric"] == "attributed", summary
            first = adapter.call("list_children", {"scan_id": scan_id, "node_id": summary["root_id"], "limit": 2})
            assert len(first["items"]) == 2 and first["next_cursor"], first
            assert first["size_metric"] == "logical", first
            second = adapter.call("list_children", {"scan_id": scan_id, "node_id": summary["root_id"], "limit": 2, "cursor": first["next_cursor"]})
            assert len({n["id"] for n in first["items"] + second["items"]}) == 4
            nodes = adapter.call("query_nodes", {"scan_id": scan_id, "descendants": True, "kind": "file"})["items"]
            expected_names = {"keep.txt", ".hidden", "permanent target.txt", "child.txt"}
            if hardlink_created:
                expected_names.add("child-hardlink.txt")
            assert {n["name"] for n in nodes} == expected_names, nodes
            attributed_children = adapter.call("list_children", {"scan_id": scan_id, "node_id": summary["root_id"], "size_metric": "attributed"})
            assert_attributed_page(attributed_children)
            attributed_files = adapter.call("query_nodes", {"scan_id": scan_id, "descendants": True, "kind": "file", "size_metric": "attributed"})
            assert_attributed_page(attributed_files)
            assert {node["id"] for node in attributed_files["items"]} == {node["id"] for node in nodes}
            assert not attributed_files["next_cursor"], attributed_files
            if hardlink_created:
                shared = [node for node in attributed_files["items"] if node["name"] in {"child.txt", "child-hardlink.txt"}]
                assert len(shared) == 2 and all(attributed_value(node) is None for node in shared), shared
                assert all(node["space_usage"]["unmeasured_entries"] > 0 for node in shared), shared
            numeric = adapter.call("query_nodes", {"scan_id": scan_id, "descendants": True, "kind": "file", "size_metric": "attributed", "min_bytes": 0})
            assert_attributed_page(numeric)
            assert numeric["excluded_unmeasured"] == sum(attributed_value(node) is None for node in attributed_files["items"]), numeric
            assert all(attributed_value(node) is not None for node in numeric["items"]), numeric
            accounting_status = assert_space_accounting(summary, attributed_files["items"])
            adapter.call("list_children", {"scan_id": scan_id, "node_id": summary["root_id"], "limit": 2, "size_metric": "attributed", "cursor": first["next_cursor"]}, error="invalid_cursor")
            target = next(n for n in nodes if n["name"] == "permanent target.txt")
            preparation = {"scan_id": scan_id, "node_ids": [target["id"]], "mode": "permanent", "request_key": "smoke-plan"}
            preparing = adapter.call("create_cleanup_plan", preparation)
            assert adapter.call("create_cleanup_plan", preparation) == preparing
            adapter.wait(preparing)
            plan = adapter.call("get_cleanup_plan", {"plan_id": preparing["plan_id"], "limit": 1})
            assert plan["id"] == preparing["plan_id"] and plan["mode"] == "permanent"
            assert len(plan["targets"]) == 1 and plan["targets"][0]["id"] == target["id"]
            adapter.call("release_scan", {"scan_id": scan_id}, error="plan_in_use")
            execution_input = {"plan_id": plan["id"], "plan_hash": plan["hash"], "request_key": "smoke-execute"}
            adapter.call("execute_trash", execution_input, error="mode_mismatch")
            assert remove.exists(), "Mode mismatch must leave the temporary target untouched"
            cleaning = adapter.call("execute_permanent", execution_input)
            assert adapter.call("execute_permanent", execution_input) == cleaning
            adapter.close()
            adapter = session.adapter()
            assert adapter.call("execute_permanent", dict(execution_input, request_key="smoke-reconnect")) == cleaning
            assert adapter.call("get_scan", {"scan_id": scan_id}).get("space_accounting") == summary.get("space_accounting")
            adapter.wait(cleaning)
            result = adapter.call("get_cleanup_result", {"job_id": cleaning["job_id"]})
            assert any(item["status"] == "succeeded" for item in result["items"]), result
            assert not remove.exists() and (root / "keep.txt").exists()
            assert result["refresh_job_id"] and result["refresh_scan_id"], result
            adapter.wait({"job_id": result["refresh_job_id"]})
            refreshed = adapter.call("get_scan", {"scan_id": result["refresh_scan_id"]})
            assert refreshed["snapshot_id"] != summary["snapshot_id"]
            assert adapter.call("get_scan", {"scan_id": scan_id})["known_stale"] is True
            refreshed_nodes = adapter.call("query_nodes", {"scan_id": refreshed["scan_id"], "descendants": True})["items"]
            assert "permanent target.txt" not in {n["name"] for n in refreshed_nodes}
            # The old immutable snapshot still describes its capture time.
            assert "permanent target.txt" in {n["name"] for n in adapter.call("query_nodes", {"scan_id": scan_id, "descendants": True})["items"]}
            if test_trash:
                trash_target = next(n for n in refreshed_nodes if n["name"] == ".hidden")
                preparing = adapter.call("create_cleanup_plan", {"scan_id": refreshed["scan_id"], "node_ids": [trash_target["id"]], "mode": "trash", "request_key": "smoke-trash-plan"})
                adapter.wait(preparing)
                trash_plan = adapter.call("get_cleanup_plan", {"plan_id": preparing["plan_id"]})
                trash_input = {"plan_id": trash_plan["id"], "plan_hash": trash_plan["hash"], "request_key": "smoke-trash"}
                adapter.call("execute_permanent", trash_input, error="mode_mismatch")
                trashed = adapter.call("execute_trash", trash_input)
                adapter.wait(trashed)
                assert not (root / ".hidden").exists()
                trash_result = adapter.call("get_cleanup_result", {"job_id": trashed["job_id"]})
                adapter.wait({"job_id": trash_result["refresh_job_id"]})
            assert adapter.call("release_scan", {"scan_id": scan_id})["released"] is True
            adapter.call("get_scan", {"scan_id": scan_id}, error="not_found")
            adapter.close()
    print(json.dumps({"passed": True, "service_mode": service_mode, "tools": len(tools), "protocol": "2025-11-25", "application_version": capabilities["application_version"], "attributed_queries": True, "hardlink_unknown_checked": hardlink_created, "space_accounting": accounting_status, "permanent_delete": True, "trash_delete": test_trash, "mode_isolation": True, "reconnect_idempotency": True, "refresh_snapshot": True, "elapsed_seconds": round(time.monotonic() - started, 3)}, ensure_ascii=False))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", type=Path, default=default_binary())
    parser.add_argument("--trash", action="store_true", help="Also move one known temporary fixture into the real OS recycle bin")
    parser.add_argument("--service-mode", choices=("auto", "supervised"), default="auto", help="Auto-start the backend, or supervise a foreground service for restricted hosts")
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error(f"Build the executable first: {args.binary}")
    run(args.binary.resolve(), args.trash, args.service_mode)
