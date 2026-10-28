#!/usr/bin/env python3
"""Exercise the real stdio adapter and shared backend using only temporary files."""
import argparse
import json
import os
from pathlib import Path
import queue
import subprocess
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
        threading.Thread(target=reader, daemon=True).start()
        threading.Thread(target=diagnostic_reader, daemon=True).start()
        initialized = self.request("initialize", {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "omd-fixture-smoke", "version": "0.1.0"}})
        assert initialized["protocolVersion"] == "2025-11-25", initialized
        assert initialized["serverInfo"]["name"] == "omd", initialized
        self.notify("notifications/initialized")

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

    def close(self):
        self.process.stdin.close()
        try:
            self.process.wait(timeout=15)
        except subprocess.TimeoutExpired:
            self.process.kill()
            raise AssertionError("MCP adapter did not exit after stdin EOF")
        assert self.process.returncode == 0, (self.process.returncode, self.stderr)
        assert not self.stderr, self.stderr


def run(binary, test_trash):
    started = time.monotonic()
    with tempfile.TemporaryDirectory(prefix="ohmydisk-mcp-") as directory:
        fixture = Path(directory).resolve()
        root = fixture / "files"
        root.mkdir()
        (root / "keep.txt").write_text("keep", encoding="utf-8")
        (root / ".hidden").write_bytes(b"hidden")
        remove = root / "permanent target.txt"
        remove.write_bytes(b"temporary deletion fixture")
        nested = root / "nested"
        nested.mkdir()
        (nested / "child.txt").write_bytes(b"nested")
        state = fixture / "state"
        adapter = Adapter(binary, state)
        tools = adapter.request("tools/list", {})["tools"]
        assert len(tools) == 17, [t["name"] for t in tools]
        assert all(tool["name"].startswith("omd_") for tool in tools), tools
        definitions = {tool["name"]: tool for tool in tools}
        for name in ("omd_execute_trash", "omd_execute_permanent"):
            assert definitions[name]["annotations"]["destructiveHint"] is True
            assert definitions[name]["annotations"]["readOnlyHint"] is False
        assert adapter.call("get_capabilities")["authorization_model"] == "host_confirmed"
        assert isinstance(adapter.call("list_sources")["items"], list)
        scan_input = {"root_path": str(root), "request_key": "smoke-scan"}
        accepted = adapter.call("start_scan", scan_input)
        assert adapter.call("start_scan", scan_input) == accepted
        adapter.call("start_scan", {"root_path": str(nested), "request_key": "smoke-scan"}, error="request_conflict")
        # Closing this adapter must not cancel the accepted backend scan.
        adapter.close()
        adapter = Adapter(binary, state)
        assert adapter.call("start_scan", scan_input) == accepted
        adapter.wait(accepted)
        scan_id = accepted["scan_id"]
        summary = adapter.call("get_scan", {"scan_id": scan_id})
        assert summary["complete"] is True and "nodes" not in summary, summary
        first = adapter.call("list_children", {"scan_id": scan_id, "node_id": summary["root_id"], "limit": 2})
        assert len(first["items"]) == 2 and first["next_cursor"], first
        second = adapter.call("list_children", {"scan_id": scan_id, "node_id": summary["root_id"], "limit": 2, "cursor": first["next_cursor"]})
        assert len({n["id"] for n in first["items"] + second["items"]}) == 4
        nodes = adapter.call("query_nodes", {"scan_id": scan_id, "descendants": True, "kind": "file"})["items"]
        assert {n["name"] for n in nodes} == {"keep.txt", ".hidden", "permanent target.txt", "child.txt"}, nodes
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
        adapter = Adapter(binary, state)
        assert adapter.call("execute_permanent", dict(execution_input, request_key="smoke-reconnect")) == cleaning
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
        print(json.dumps({"passed": True, "tools": len(tools), "protocol": "2025-11-25", "permanent_delete": True, "trash_delete": test_trash, "mode_isolation": True, "reconnect_idempotency": True, "refresh_snapshot": True, "elapsed_seconds": round(time.monotonic() - started, 3)}, ensure_ascii=False))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", type=Path, default=default_binary())
    parser.add_argument("--trash", action="store_true", help="Also move one known temporary fixture into the real OS recycle bin")
    args = parser.parse_args()
    if not args.binary.is_file():
        parser.error(f"Build the executable first: {args.binary}")
    run(args.binary.resolve(), args.trash)
