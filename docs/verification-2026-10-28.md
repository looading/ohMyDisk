# ohMyDisk 验证记录

日期：2026-10-28

本记录包括初期本机验证和后续 GitHub 原生验证。当前已在 Windows x64、macOS Apple Silicon 与 Intel runner 上完成原生构建、测试、MCP 清理和产物上传。

## GitHub 原生验证

[完整 CI run 40686597761](https://github.com/looading/ohMyDisk/actions/runs/40686597761) 对提交 3709886 的三个原生任务均已通过。

| 平台 | Rust 测试 | 打包测试 | 真实 MCP | 产物 |
|---|---|---|---|---|
| Windows x64 | 27 通过 | 6 通过 | supervised 服务，17 工具及两种真实清理通过 | ZIP 与 SHA256 已上传 |
| macOS Apple Silicon | 28 通过 | 6 通过 | auto 服务，17 工具及两种真实清理通过 | ZIP 与 SHA256 已上传 |
| macOS Intel | 28 通过 | 6 通过 | auto 服务，17 工具及两种真实清理通过 | ZIP 与 SHA256 已上传 |

三个任务都执行格式检查、Clippy、锁定依赖构建、模式隔离、客户端重连、幂等与快照刷新验证。Windows 的正常本地文件回收已实际调用系统 RecycleItem，不是仅编译检查。

GitHub Windows runner 禁止进程脱离 Job Object，因此 CI 显式监督自己的前台临时服务。自动后台服务的脱离要求未放宽；受限宿主可以用 service --stay-alive 连接同一套服务。Windows GUI 人工走查及复杂回收场景仍是单独的覆盖边界。

[v0.1.0 发布运行 40688363332](https://github.com/looading/ohMyDisk/actions/runs/40688363332) 随后再次通过三个原生任务和 Release 发布。正式 Release 包含三份 ZIP 和三份 SHA256 文件，均已下载复核校验值、包内架构及说明文档；macOS 执行权限有效，Windows PE 为 x86_64，未发现动态 Visual C++ 运行库依赖。源码、公开 Git 身份及三份正式产物的全部归档内容均完成公司标识和私有主目录路径检查，无命中。

## 初期本机验证

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

上面的本机方法不能替代原生构建。后续 windows-2022 CI 已实际完成 bundled SQLite C 构建、链接、workspace tests、release build 和 supervised 模式的 --trash MCP smoke，结果见本记录的 GitHub 原生验证部分。

不同 Windows 回收站命名空间、卷支持、禁用回收站、容量不足、长路径与 GUI 仍需额外覆盖。实现与系统契约说明见 [Windows 平台说明](../crates/ohmydisk-platform/WINDOWS.md)。

## 扫描基线

本机 APFS、debug 构建、100 个子目录中的 10,000 个文件，共 1,280,000 逻辑字节。包含后台启动与轮询的时间为 0.505 秒，采样到的服务 RSS 为 42,976 KiB。RSS 是轮询观测值，不保证捕获进程真实峰值；这个文件集不代表全盘扫描性能。

原始数据见 [基线 JSON](benchmark-2026-10-28.json)。
