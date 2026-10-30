# OhMyDisk

OhMyDisk 是用 Rust 和 iced 开发的本地磁盘空间分析工具，提供桌面界面和 MCP 接口，用于扫描文件、查看占用和核对清理计划。

[English](README.md) | [简体中文](README.zh-CN.md)

[![构建与打包](https://github.com/looading/ohMyDisk/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/looading/ohMyDisk/actions/workflows/ci.yml)

## 下载

当前开发构建可从 [Actions 的 Artifacts](https://github.com/looading/ohMyDisk/actions) 下载，已打标签的版本见 [GitHub Releases](https://github.com/looading/ohMyDisk/releases)。当前构建生成的 ZIP 包含中英文 README 和 MCP 文档，并提供单独的 `.sha256` 校验文件。

| 平台 | 当前构建的 ZIP |
|---|---|
| Windows x64 | `OhMyDisk-windows-x86_64.zip` |
| macOS Apple Silicon | `OhMyDisk-macos-aarch64.zip` |
| macOS Intel | `OhMyDisk-macos-x86_64.zip` |

解压后，macOS 打开 `OhMyDisk.app`，Windows 运行 `ohmydisk.exe`。macOS 要求 13 或更高版本。

## 功能

- 扫描本地磁盘或文件夹，包含隐藏文件，记录权限错误和未遍历的边界。
- 用联动的旭日图和目录列表查看占用，支持按大小排序、悬停预览和路径导航。
- 在顶栏随时切换 **English** 和 **中文**。首次启动默认英文，下次启动保留上次选择。
- 从图表或目录列表拖入待清理集合，拖动分隔条调整收集区高度。
- 先核对精确目标，再移入系统回收站；永久删除单独确认。
- 查看和取消后台作业，重新打开历史扫描；关闭 GUI 后，仍可通过 stdio MCP 使用同一个本地服务。

容量视图将文件系统总容量、可用空间、已确认文件占用，以及共享、系统和未归属空间放在同一张图中。逻辑大小保留在详情和 API 中，稀疏文件或共享区块不会把图表总量撑到磁盘容量之外。无法测量的项目会明确标记为未知。

扫描默认不跟随符号链接、junction 和未知 reparse point，不自动提权。未完整扫描的目录不能按完整目录清理。已确认占用不等于清理后一定释放的容量，移入回收站也可能不改变可用空间。

![容量视图，使用合成验收数据](docs/images/space-view-0.2.0.png)

图中使用合成数据：995 GB 总容量、245 GB 可用、500 GB 已确认文件占用，以及 250 GB 共享、系统及未归属空间。截图展示中文界面。

## 构建与运行

要求 **Rust 1.88+**，桌面界面使用 **iced 0.14**。

```sh
cargo run -p ohmydisk --release
```

可执行文件默认打开 GUI，也可以运行 MCP 或执行诊断命令：

```sh
cargo run -p ohmydisk --release -- mcp --stdio
cargo run -p ohmydisk --release -- call get_capabilities
```

状态默认保存在当前用户的本地应用数据目录。通过 `--data-dir` 或 `OHMYDISK_DATA_DIR` 指定其他目录，便于隔离测试：

```sh
cargo run -p ohmydisk --release -- --data-dir /absolute/path/to/test-state
```

语言选择保存在 `<data-dir>/preferences.json`。切换后界面立即更新，下次启动继续使用所选语言。

## Codex 与其他 MCP 客户端

构建可执行文件或下载压缩包后，将程序的绝对路径加入客户端 MCP 配置：

```toml
[mcp_servers.omd]
command = "/absolute/path/to/ohmydisk"
args = ["mcp", "--stdio"]
```

macOS 打包程序的路径为 `OhMyDisk.app/Contents/MacOS/ohmydisk`。Windows 使用 `ohmydisk.exe` 的实际路径。

客户端必须先展示清理计划，再分别为 `omd_execute_trash` 和 `omd_execute_permanent` 请求人工批准。永久删除不可恢复。MCP 执行依赖客户端确认，应用不会再弹出 GUI 确认框。

工具、大小统计口径、调用示例，以及受限 Windows 宿主的前台服务启动方式，见 [MCP 接入文档](docs/mcp.md)。

## 测试与打包

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets
cargo test --workspace
cargo build --release -p ohmydisk
python3 -m unittest discover -s tests -p 'test_*.py'
python3 tests/mcp_smoke.py
```

MCP smoke 使用隔离的临时文件和状态目录，清理测试只操作测试创建的文件。CI 分别在 Windows x64、macOS Apple Silicon 和 macOS Intel 原生 runner 上构建和测试。

打包要求 **Python 3.11+**，并在 Windows 或 macOS 本机运行：

```sh
python3 scripts/package.py
```

脚本构建 release 程序，在 `dist/` 生成 ZIP 和 SHA-256 校验文件。已有 release 构建时，可加 `--skip-build`。macOS 应用使用供本机运行的 ad-hoc 签名，Windows 程序未签名；发布签名和 macOS 公证需要发布方自己的证书。

## 更多资料

- [MCP 接口与容量统计](docs/mcp.md)
- [扫描基准与测量命令](https://github.com/looading/ohMyDisk/tree/main/benchmarks)

发布时推送与 workspace 版本一致的标签，例如 `v0.2.0`。所有原生构建与测试通过后，CI 会将各平台 ZIP 和校验文件发布到 GitHub Releases。
