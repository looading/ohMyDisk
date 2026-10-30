# OhMyDisk

OhMyDisk is a local disk-space analyzer built with Rust and iced, with an interactive desktop app and MCP tools for scanning, exploring, and reviewing cleanup plans.

[English](README.md) | [简体中文](README.zh-CN.md)

[![Build and packages](https://github.com/looading/ohMyDisk/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/looading/ohMyDisk/actions/workflows/ci.yml)

## Download

Get current development builds from [Actions → Artifacts](https://github.com/looading/ohMyDisk/actions), or tagged versions from [GitHub Releases](https://github.com/looading/ohMyDisk/releases). ZIPs produced by current builds include both READMEs and the MCP guide, with a separate `.sha256` checksum file.

| Platform | Current build ZIP |
|---|---|
| Windows x64 | `OhMyDisk-windows-x86_64.zip` |
| macOS Apple Silicon | `OhMyDisk-macos-aarch64.zip` |
| macOS Intel | `OhMyDisk-macos-x86_64.zip` |

Extract the archive, then open `OhMyDisk.app` on macOS or `ohmydisk.exe` on Windows. macOS requires version 13 or later.

## What it does

- Scan local disks or folders, including hidden files, while reporting permission errors and skipped boundaries.
- Explore disk usage with a linked sunburst chart and size-sorted directory list, hover previews, and breadcrumb navigation.
- Switch between **English** and **中文** in the top bar at any time. English is the default; your choice is saved for the next launch.
- Drag chart segments or directory rows into the collection, then resize the collection to review more items.
- Review exact cleanup targets before moving them to the system trash. Permanent deletion is a separate, explicitly confirmed action.
- Track and cancel background jobs, reopen saved scans, and use the same local service through stdio MCP while the GUI is closed.

The capacity view accounts for filesystem capacity as available space, confirmed file usage, and shared/system/unattributed space. Logical file sizes remain available in details and the API; sparse files and shared blocks do not inflate the chart beyond disk capacity. Unknown measurements stay marked as unknown.

Scans do not follow symbolic links, junctions, or unknown reparse points, and do not elevate privileges automatically. Incompletely scanned directories cannot be cleaned as complete directories. Confirmed usage is not a promise of space recovered; moving files to trash may leave available capacity unchanged.

![Capacity view with synthetic acceptance data](docs/images/space-view-0.2.0.png)

The example uses synthetic data: 995 GB capacity, 245 GB available, 500 GB confirmed file usage, and 250 GB shared/system/unattributed space. The screenshot shows the Chinese interface.

## Build and run

Requires **Rust 1.88+**. The desktop UI uses **iced 0.14**.

```sh
cargo run -p ohmydisk --release
```

The executable opens the GUI by default. It also provides MCP and diagnostic commands:

```sh
cargo run -p ohmydisk --release -- mcp --stdio
cargo run -p ohmydisk --release -- call get_capabilities
```

State is stored in the current user's local application-data directory. Use `--data-dir` or `OHMYDISK_DATA_DIR` to select another directory, for example when testing:

```sh
cargo run -p ohmydisk --release -- --data-dir /absolute/path/to/test-state
```

The language setting is saved in `<data-dir>/preferences.json`. Switching languages updates the running app immediately and applies again on the next launch.

## Codex and other MCP clients

Build the executable or use a downloaded package, then add its absolute path to your client's MCP configuration:

```toml
[mcp_servers.omd]
command = "/absolute/path/to/ohmydisk"
args = ["mcp", "--stdio"]
```

On macOS, the bundled executable is `OhMyDisk.app/Contents/MacOS/ohmydisk`. On Windows, use the path to `ohmydisk.exe`.

The client must show the cleanup plan and request approval for `omd_execute_trash` and `omd_execute_permanent` separately. Permanent deletion cannot be undone. MCP execution relies on the client's confirmation and does not open another GUI dialog.

See the [MCP guide (Simplified Chinese)](docs/mcp.md) for tools, size metrics, examples, and the foreground-service workaround for restricted Windows hosts.

## Test and package

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --release -p ohmydisk
python3 -m unittest discover -s tests -p 'test_*.py'
python3 tests/mcp_smoke.py
```

The MCP smoke test uses isolated temporary files and a separate state directory. Cleanup tests operate only on files created by the tests. CI builds and tests Windows x64 and both macOS architectures on native runners.

Packaging requires **Python 3.11+** and a native Windows or macOS runner:

```sh
python3 scripts/package.py
```

The script builds a release executable and writes the ZIP and SHA-256 file to `dist/`. Use `--skip-build` to package an existing release build. macOS bundles have an ad-hoc signature for local use; Windows executables are unsigned. Distribution signing and macOS notarization require the publisher's certificates.

## More information

- [MCP reference and capacity accounting (Simplified Chinese)](docs/mcp.md)
- [Scan benchmarks and measurement commands](https://github.com/looading/ohMyDisk/tree/main/benchmarks)

To publish a release, push a tag matching the workspace version, such as `v0.2.0`. After all native builds and tests pass, CI publishes the platform ZIPs and checksums to GitHub Releases.
