# ohMyDisk 验证记录

日期：2026-10-28

本轮交付可运行的 macOS 应用、本地 MCP 服务、Windows 平台实现与 CI 配置。Windows 已进行 Rust 跨目标检查，尚无 Windows 链接与运行证据。

## 本机验证

| 检查 | 实际结果 |
|---|---|
| cargo test --workspace | 26 项测试通过，0 失败；包括旭日图 3、核心 7、平台 4、IPC 6、服务生命周期 6 |
| cargo fmt --all --check | 通过 |
| cargo clippy --workspace --all-targets | 退出码 0；仍有 pedantic 风格、公开 API 文档及显式 FFI unsafe 提示，未声称零警告 |
| cargo build --release -p ohmydisk | 本机原生构建通过 |
| 真实 MCP 子进程 | 17 个工具，协议 2025-11-25，结构化输出，无协议外 stdout |
| MCP 临时文件清理 | 实际永久删除与 NSFileManager 废纸篓移动通过；模式隔离、重连去重和刷新新快照通过 |
| GUI 实际窗口 | 应用启动、中文字体、共享扫描历史、旭日图、按大小列表、按钮加入收集与计划预览确认界面已观察 |
| GUI 拖放 | 代码与事件分发已检查；自动化手势尚未获得成功证据，保留加入按钮 |

测试只操作隔离的临时目录和测试创建的文件。GUI 预览未确认执行清理；真实清理由隔离的 MCP 测试覆盖。

## 已修复的审查问题

- 客户端长时间空闲后后台服务退出，旧 Client 不能继续调用。现在只在请求尚未发送的连接阶段自动恢复；发送后的响应丢失不自动重放。
- macOS 路径大小写别名绕过保护。现在使用平台文件身份与祖先身份，区分仅保护目录本身与保护整棵子树。
- 校验或写入 started 记录期间收到取消，之后仍开始清理。两种执行方式均在调用系统操作前再次检查。
- 清理中断后旧扫描状态未标旧。恢复保守标旧，并保留未知结果，不续跑删除。
- 清理结果清单尚在初始化时开放偏移分页。现在等待成员集合固定，之后只更新已有成员。
- macOS 默认 Finder 回收方式等待 Automation 授权。改用 NSFileManager。
- Windows 通用 DeleteItem 的“尽可能回收”无法保证不永久删除。改为专用 RecycleItem，不支持时失败，不降级。

## Windows 检查的实际边界

已对 x86_64-pc-windows-msvc 执行整个 workspace 与所有测试目标的 cargo check。为在 macOS 上完成 Rust cfg/API 检查，该次检查通过临时环境跳过了 bundled SQLite 的 C 构建，仅使用 crate 自带 Rust bindings 与链接声明；没有提供 Windows SQLite 库，也没有链接可执行文件。

这不能替代 Windows 原生构建、ABI 与运行验证。CI 的 windows-latest 包含正常 bundled SQLite 构建、workspace tests、release build 和带 --trash 的 MCP smoke；当前仓库未连接远端，流水线尚未执行。

Windows 回收站的命名空间、卷支持、禁用回收站、容量不足、长路径与 GUI 需要 Windows 实体机或 CI 验证。实现与系统契约说明见 [Windows 平台说明](../crates/ohmydisk-platform/WINDOWS.md)。

## 扫描基线

本机 APFS、debug 构建、100 个子目录中的 10,000 个文件，共 1,280,000 逻辑字节。包含后台启动与轮询的时间为 0.505 秒，采样到的服务 RSS 为 42,976 KiB。RSS 是轮询观测值，不保证捕获进程真实峰值；这个文件集不代表全盘扫描性能。

原始数据见 [基线 JSON](benchmark-2026-10-28.json)。
