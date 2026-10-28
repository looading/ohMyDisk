# ohMyDisk

[![Build and packages](https://github.com/looading/ohMyDisk/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/looading/ohMyDisk/actions/workflows/ci.yml)

ohMyDisk 使用 Rust 和 iced 分析本地磁盘空间，提供旭日图、按大小排列的文件列表与明确的清理计划。GUI 和 MCP 共用按需启动的本地服务，Codex 等 agent 可以在 GUI 未打开时使用扫描与清理能力。

## 下载

版本发布后，可从 [GitHub Releases](https://github.com/looading/ohMyDisk/releases) 下载对应平台的 ZIP。每份 ZIP 都提供同名 `.sha256` 校验文件，并包含使用说明和 MCP 接入文档。

| 平台 | 产物 |
|---|---|
| Windows x64 | [ohMyDisk-windows-x86_64.zip](https://github.com/looading/ohMyDisk/releases/latest/download/ohMyDisk-windows-x86_64.zip) |
| macOS Apple Silicon | [ohMyDisk-macos-aarch64.zip](https://github.com/looading/ohMyDisk/releases/latest/download/ohMyDisk-macos-aarch64.zip) |
| macOS Intel | [ohMyDisk-macos-x86_64.zip](https://github.com/looading/ohMyDisk/releases/latest/download/ohMyDisk-macos-x86_64.zip) |

普通分支构建也可以从 [Actions](https://github.com/looading/ohMyDisk/actions) 对应运行的 Artifacts 下载。macOS 解压后打开包内的 `ohMyDisk.app`，Windows 运行 `ohmydisk.exe`。

## 运行

要求 Rust 1.88 或更高版本。当前使用 iced 0.14 与 rmcp 3.8。

```sh
cargo run -p ohmydisk --release
```

命令行入口默认打开 GUI，也可以运行本地 MCP 或执行诊断请求。

```sh
cargo run -p ohmydisk --release -- mcp --stdio
cargo run -p ohmydisk --release -- call get_capabilities
```

默认状态保存在当前用户的本地应用数据目录内。`--data-dir` 或 `OHMYDISK_DATA_DIR` 可以指定独立状态目录，适合测试。

## 功能

- 本地磁盘与文件夹扫描，包含隐藏文件，记录权限错误和未遍历的边界。
- 旭日图、列表联动、目录下钻、路径导航与按大小浏览。
- 待清理集合、不可变计划、默认移入回收站和独立的永久删除。
- 后台作业进度与取消，冻结扫描快照与稳定分页查询。
- 本地 stdio MCP，完整的扫描、查询、制订计划和清理流程。

默认不跟随符号链接、junction 和未知 reparse point，不自动提权。无法完整扫描的目录不能作为完整目录清理。逻辑大小、已选大小和磁盘实际可用空间分别展示，移入回收站不表示容量已经释放。

GUI 执行清理时显示应用内确认。MCP 信任客户端完成人工确认：必须先展示计划，再批准对应的回收站或永久删除工具；应用不再增加第二次弹窗。永久删除不可恢复。

## Codex 与其他 MCP 客户端

先构建可执行文件，再将其绝对路径加入客户端。详见 [MCP 接入](docs/mcp.md)。配置示例：

```toml
[mcp_servers.omd]
command = "/absolute/path/to/ohmydisk"
args = ["mcp", "--stdio"]
```

Windows 使用 `ohmydisk.exe` 的实际绝对路径。两个执行工具需要各自在宿主中启用人工审批，不能将整个服务设为无条件批准。

## 验证与构建

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --release -p ohmydisk
python3 tests/mcp_smoke.py
```

MCP smoke 使用隔离临时文件与状态目录，不读取用户目录。清理测试只对测试创建的文件执行。GitHub Actions 在 Windows x64、macOS Apple Silicon 和 macOS Intel 上分别运行原生构建与测试。

```sh
python3 scripts/package.py
```

打包脚本在 `dist/` 创建按操作系统与架构命名的 ZIP 和校验文件。macOS 应用只有用于本机运行的 ad-hoc 签名，Windows 程序未签名；发布证书签名与 macOS 公证需要发布方自己的证书。

## 发布流程

推送 `main`、提交 Pull Request 或手动运行 workflow 都会构建三个平台的独立 Actions 产物。推送与 `Cargo.toml` 版本一致的 `v*` 标签时，所有构建与测试通过后自动创建 GitHub Release，附上三个 ZIP 和各自的 SHA-256 文件。

例如，workspace 版本为 `0.1.0` 时，发布标签为 `v0.1.0`。标签版本不匹配会拒绝发布。

设计和执行进展见 [功能与接口设计](docs/superpowers/specs/2026-10-28-ohmydisk-design.md) 与 [实现计划](docs/superpowers/plans/2026-10-28-ohmydisk-implementation.md)。
