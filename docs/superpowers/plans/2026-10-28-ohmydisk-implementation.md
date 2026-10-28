# ohMyDisk Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付能够在 macOS 与 Windows 上运行的磁盘扫描与清理应用，提供 iced GUI 和可独立使用的本地 MCP 入口。

**Architecture:** 采用按需启动的单用户本地服务管理后台作业、快照、清理计划和持久化。GUI 与 MCP 适配器共用服务，核心用例通过平台接口访问文件系统能力。每个任务在同一项目内实施，独立文件由并行 worker 负责，集成与验证由主代理完成。

**Tech Stack:** Rust edition 2024，最低 Rust 1.88，iced 0.14，Tokio，serde，SQLite，rmcp；平台实现使用 Rust 标准库和系统适配 crate。

**Execution status:** 核心、平台、本地服务、MCP 和 GUI 已实现。Windows x64、macOS Apple Silicon 与 Intel 的原生构建、单测和真实 MCP 清理均已通过 GitHub CI，详情见 ../../verification-2026-10-28.md。Windows GUI 人工走查、复杂回收场景和图表拖放仍需独立验收。


## 实施结果

- [x] 扫描核心、稳定查询与原生路径保真。
- [x] macOS 与 Windows 平台实现及整树保护。
- [x] 不可变清理计划、两种方式、重复请求与未知结果恢复。
- [x] 本地共享服务、连接恢复、GUI 与 MCP 入口。
- [x] 本机 26 项测试与真实临时文件 MCP 清理验证。
- [x] macOS release 应用与 ZIP，以及两平台 CI 配置。
- [x] Windows 原生链接、真实 MCP 扫描及系统回收，已通过 CI 的受监督服务模式。
- [ ] Windows 图形界面与复杂回收场景的人工验收。
- [ ] 图表拖放手势实际验收，按钮加入收集已验证。

以下保留原执行步骤，实际证据以验证记录为准。

## Global Constraints

- 使用 Rust、iced 和 Rust 生态能力实现。
- 首版支持 macOS 与 Windows，通过平台适配层保留新增平台的能力。
- MCP 覆盖完整流程，Codex 等 AI agent 能够在 GUI 未打开时扫描、查询、制订计划和执行已授权的清理。
- 默认移入系统废纸篓或回收站，同时提供单独授权的永久删除。
- MCP 清理信任 agent 宿主完成人工确认，ohMyDisk 不增加第二次弹窗或终端确认。
- 不自动提权、不跟随目录链接、不静默降级永久删除；验证清理时只使用临时目录。
- 不更改当前 Codex MCP 配置；提供用户可安装的配置示例。
- 每个 Cargo crate 使用 unsafe_code = warn、clippy all/pedantic = warn，平台 FFI 的 unsafe 代码具有明确边界和说明。

## 文件与接口

项目使用 Cargo workspace。根 Cargo.toml 管理版本，Cargo.lock 提交到仓库。各模块的职责如下。

| 路径 | 职责 |
|---|---|
| crates/ohmydisk-core/src/model.rs | 可序列化的路径、扫描节点、快照、计划、作业与结果模型 |
| crates/ohmydisk-core/src/platform.rs | Platform trait 和文件元数据契约 |
| crates/ohmydisk-core/src/scan.rs | 不跟随链接的递归扫描、统计、问题与取消 |
| crates/ohmydisk-core/src/query.rs | 冻结快照的稳定分页、范围查询和聚合 |
| crates/ohmydisk-core/src/cleanup.rs | 计划规范化、目标复核和两种执行方式 |
| crates/ohmydisk-platform/src/lib.rs | NativePlatform、磁盘枚举、回收站和定位 |
| crates/ohmydisk-platform/src/windows.rs | Windows 文件身份、分配大小、reparse 信息 |
| crates/ohmydisk-service/src/store.rs | SQLite 状态、请求去重和异常恢复 |
| crates/ohmydisk-service/src/engine.rs | 作业、扫描、计划与清理生命周期 |
| crates/ohmydisk-service/src/ipc.rs | 本地服务发现、单实例协调、IPC 与客户端 |
| crates/ohmydisk-service/src/api.rs | GUI 与 MCP 共用的请求分发及返回值 |
| crates/ohmydisk/src/main.rs | gui、mcp、service 和诊断子命令 |
| crates/ohmydisk/src/mcp.rs | 17 个工具的 schema、stdio 生命周期和错误映射 |
| crates/ohmydisk/src/gui/mod.rs | iced 状态、任务和页面布局 |
| crates/ohmydisk/src/gui/sunburst.rs | 环形树布局、命中测试、悬停与点击 |
| tests/mcp_smoke.py | 真实 stdio 握手与工具调用集成验证 |
| .github/workflows/ci.yml | macOS 与 Windows 的构建和测试 |

核心接口在任务开始时固定为以下形式，模型字段由 model.rs 定义，模块间不互相复制 DTO。

```rust
pub trait Platform: Send + Sync {
    fn metadata(&self, path: &std::path::Path) -> std::io::Result<FileInfo>;
    fn volumes(&self) -> std::io::Result<Vec<Volume>>;
    fn trash(&self, path: &std::path::Path) -> std::io::Result<()>;
    fn reveal(&self, path: &std::path::Path) -> std::io::Result<()>;
    fn protected_paths(&self) -> Vec<std::path::PathBuf>;
}

pub fn scan(
    root: &std::path::Path,
    platform: &dyn Platform,
    cancelled: &std::sync::atomic::AtomicBool,
    progress: &mut dyn FnMut(ScanProgress),
) -> Result<Snapshot, AppError>;

pub fn prepare_plan(
    snapshot: &Snapshot,
    node_ids: &[String],
    mode: CleanupMode,
    protected: &[std::path::PathBuf],
) -> Result<CleanupPlan, AppError>;

pub fn execute_plan(
    plan: &CleanupPlan,
    platform: &dyn Platform,
    cancelled: &std::sync::atomic::AtomicBool,
    record: &mut dyn FnMut(ItemResult) -> Result<(), AppError>,
) -> Result<Vec<ItemResult>, AppError>;
```

## Task 1: 项目基础与可执行的扫描核心

**Files:** 根 Cargo.toml、rust-toolchain.toml、.gitignore；core 的 model.rs、platform.rs、scan.rs、query.rs 与 tests/scan.rs。

**Consumes:** 设计中冻结快照、逻辑大小、完整性和路径身份的语义。

**Produces:** Platform、FileInfo、NativePath、Node、Snapshot、ScanProgress、AppError，以及 scan 和节点分页查询。

- [ ] 初始化 Git 的实现分支，保留已批准的设计和计划文档；创建 workspace 与核心 crate。
- [ ] 定义 NativePath 的 Unix 原始字节和 Windows UTF-16 序列化，显示路径只用于展示；定义 NodeKind、FileInfo、Snapshot 与错误类型。
- [ ] 先加入临时目录扫描测试并运行，确认扫描实现缺失时测试失败。

```rust
#[test]
fn includes_hidden_files_and_sums_directory_bytes() {
    let fixture = tempfile::tempdir().unwrap();
    std::fs::create_dir(fixture.path().join(".hidden")).unwrap();
    std::fs::write(fixture.path().join(".hidden/data"), b"12345").unwrap();
    let result = scan(fixture.path(), &TestPlatform, &false.into(), &mut |_| {}).unwrap();
    assert_eq!(result.nodes[0].logical_bytes, 5);
    assert!(result.nodes.iter().any(|node| node.name == ".hidden"));
}
```

- [ ] 实现迭代遍历、后序汇总、同卷边界、链接跳过、问题记录和协作取消；按批次回报进度。
- [ ] 实现按逻辑大小及节点 ID 稳定排序的分页，cursor 绑定快照与查询签名；按范围、名称、类型、大小和修改时间查询。
- [ ] 运行 cargo test -p ohmydisk-core，验证隐藏目录、链接、分页、取消与错误语义；通过后保存提交。

## Task 2: 原生平台能力

**Files:** platform crate、core 的平台测试辅助对象。

**Consumes:** Platform、FileInfo、Volume 与 NativePath。

**Produces:** NativePlatform，实现 metadata、volumes、trash、reveal、protected_paths。

- [ ] 验证 macOS 的 dev/ino、blocks 与 Windows 卷 ID/文件 ID/reparse/分配大小的来源，未知值保留为空。
- [ ] 实现不跟随链接的 metadata 与系统磁盘枚举，系统回收站通过受维护 crate 调用；定位使用参数化系统能力，不拼接 shell。
- [ ] 检查本机临时文件 metadata 与 std 元数据相符，并验证隐藏文件和符号链接。
- [ ] 在隔离临时目录执行一次回收站移动能力检查；不支持时返回 unsupported，禁止改为永久删除。
- [ ] 运行 cargo test -p ohmydisk-platform，通过后保存提交。

## Task 3: 不可变清理计划与执行

**Files:** core/cleanup.rs、core/tests/cleanup.rs。

**Consumes:** 完成的 Snapshot、Platform 与规范化原生路径。

**Produces:** CleanupPlan、CleanupMode、ItemResult，prepare_plan 和 execute_plan。

- [ ] 添加父子重复目标测试：同时选择父目录和其文件时只有父目标，成员去重；选择根目录、受保护目录或不完整目录应失败。
- [ ] 实现计划 hash、10 分钟有效期、文件身份与目录成员清单，限制输入为最多 500 个节点。
- [ ] 加入先准备计划再新增文件的回归场景，执行返回 stale_plan，新增文件必须仍存在。
- [ ] 永久删除按授权成员自下而上执行，目录非空时保留；回收站只调用 trash，失败不回退。
- [ ] 每个系统操作前回调记录 started，再回调最终 ItemResult；记录失败时禁止继续系统操作。
- [ ] 测试 mode 不匹配、文件替换、目录新增、过期、取消和部分失败。运行 cargo test -p ohmydisk-core，通过后保存提交。

## Task 4: 持久化作业与共享本地服务

**Files:** service crate 的 store.rs、engine.rs、api.rs、ipc.rs 与 tests/lifecycle.rs。

**Consumes:** core 扫描、查询、清理接口与 NativePlatform。

**Produces:** Engine、Client，以及以工具名和 JSON 参数调用的统一服务接口。

```rust
impl Client {
    pub async fn connect_or_spawn(data_dir: std::path::PathBuf) -> Result<Self, AppError>;
    pub async fn call(&self, operation: &str, arguments: serde_json::Value)
        -> Result<serde_json::Value, AppError>;
}
```

- [ ] SQLite 保存快照、计划、作业、逐项结果和带输入摘要的 request_key；进程启动后将遗留 running 状态改为 interrupted，started 项改为 in_doubt。
- [ ] 加入相同请求键重复启动只返回同一作业、输入变化返回 request_conflict 的测试，然后实现事务认领。
- [ ] 使用阻塞 worker 执行扫描、计划准备和清理，job 查询不等待长任务；作业状态包含 cancel_requested。
- [ ] 所有查询限制分页大小；结果释放检查有效计划与活跃作业引用。
- [ ] 清理终止后标记旧扫描 known_stale，排队扫描原根，并在结果中提供刷新 job 引用。
- [ ] 实现 Unix socket / Windows pipe、本用户连接边界、协议版本、单实例启动竞争与独立服务生命周期。
- [ ] 运行 cargo test -p ohmydisk-service，并实测两个独立客户端连接同一服务、一个断开后另一个仍能查询作业；通过后保存提交。

## Task 5: MCP 与命令入口

**Files:** app main.rs、mcp.rs、tests/mcp_smoke.py、docs/mcp.md。

**Consumes:** Client::call 与 17 个操作的统一服务契约。

**Produces:** ohmydisk mcp --stdio、ohmydisk service、诊断 JSON 入口，以及真实 MCP tools/list/tools/call。

- [ ] 先用临时最小服务验证 rmcp 与 Codex 使用的握手版本，依据实际握手锁定 SDK。
- [ ] 定义设计中的 17 个工具及 inputSchema，读工具标记 readOnly，两个 execute 标记 destructive；结构化对象与文本 JSON 同时返回。
- [ ] CLI 使用 clap；默认运行 GUI；mcp 模式不得向 stdout 写诊断。
- [ ] 用隔离数据目录与临时文件实际发送 initialize、tools/list、start_scan、get_job、query_nodes、prepare plan、execute 和 result 请求。
- [ ] 验证垃圾桶与永久删除工具模式隔离、MCP 错误返回、stdout 无污染和重连。
- [ ] 运行 cargo test --workspace 与 python3 tests/mcp_smoke.py，通过后保存提交。

## Task 6: iced GUI 与旭日图交互

**Files:** app gui/mod.rs、gui/sunburst.rs、GUI 测试与 README。

**Consumes:** Client::call 的相同作业、节点与计划数据。

**Produces:** 磁盘首页、扫描状态、分析页面、联动列表与旭日图、收集区和清理结果。

- [ ] 原生目录选择器启动扫描，异步轮询作业，不在 UI 线程遍历文件。
- [ ] 实现目录面包屑、历史前后导航、按大小排序列表和小文件聚合；hover 不改变导航。
- [ ] 旭日图按每层节点字节比例排布扇区，返回节点命中；悬停联动，点击目录下钻。
- [ ] 收集区支持加入、移出、清空、合计与父子去重；既提供拖放，也保留显式加入按钮。
- [ ] GUI 内执行前确认精确目标及方式；默认回收站，永久删除独立确认；显示逐项结果和刷新状态。
- [ ] 用固定临时目录运行真实 GUI，检查空态、扫描、导航、图表命中、收集和取消；清理仅使用临时测试文件。
- [ ] 对旭日图角度、层级、命中和大小为零情况做纯逻辑测试；运行 cargo test --workspace 和 cargo build，通过后保存提交。

## Task 7: 交付与整体验证

**Files:** README.md、docs/mcp.md、.github/workflows/ci.yml、打包脚本与示例配置。

**Consumes:** 可运行的 GUI、本地服务和 MCP 二进制。

**Produces:** 开发运行命令、安装构建产物、Codex 配置示例和平台验证记录。

- [ ] 配置 macOS 与 Windows CI：fmt、clippy、workspace tests、release build，上传对应二进制。
- [ ] 本机运行 cargo fmt --all --check、cargo clippy --workspace --all-targets、cargo test --workspace 和 release build。
- [ ] 运行真实 MCP smoke 与本地服务重连测试；对大临时目录记录扫描时间和峰值内存。
- [ ] 独立 reviewer 按已批准设计检查删除边界、幂等、恢复、IPC 与 GUI 状态；修复重要缺陷后重跑受影响验证。
- [ ] README 写明真实运行与配置步骤，区分本机已验证与需 Windows 运行验证的部分；不将配置示例称为已接入当前 Codex。
- [ ] 更新本文任务状态，并提供实际运行结果、产物路径及残留限制。
