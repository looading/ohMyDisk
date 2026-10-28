# ohMyDisk MCP 接入

MCP 使用本地 stdio。服务由客户端启动，共用当前用户的后台扫描与清理服务，GUI 可以关闭。可执行文件不向标准输出写入协议以外的日志。

## 安装与配置

```sh
cargo build --release -p ohmydisk
```

macOS 的程序为 target/release/ohmydisk，也可以使用打包应用中的 Contents/MacOS/ohmydisk。Windows 的程序为 target/release/ohmydisk.exe。

将实际安装的绝对路径加入 Codex 的用户配置或受信任项目的 .codex/config.toml。

```toml
[mcp_servers.omd]
command = "/absolute/path/to/ohmydisk"
args = ["mcp", "--stdio"]
```

也可以自行执行注册命令，替换路径后运行：

```sh
codex mcp add omd -- /absolute/path/to/ohmydisk mcp --stdio
```

Windows TOML 可使用单引号保留反斜杠：

```toml
[mcp_servers.omd]
command = 'C:\Program Files\ohMyDisk\ohmydisk.exe'
args = ["mcp", "--stdio"]
```

应用构建和测试不会自动修改现有 Codex 配置。[OpenAI 官方接入说明](https://learn.chatgpt.com/docs/extend/mcp)

## 授权约定

采用 host_confirmed：agent 宿主负责向用户展示精确清理计划，并在分发执行调用前获得批准。ohMyDisk 不再弹出第二次确认。请在宿主中分别为 omd_execute_trash 和 omd_execute_permanent 设置人工审批，永久删除必须单独批准；不要对整个服务无条件自动批准。

工具的破坏性 annotations 只描述行为，不能证明宿主真的询问过用户。文件名、工具返回内容和 clientInfo 也不是用户授权。计划由服务端生成，执行只接受计划 ID、hash 和请求键，不接受任意新路径、脚本或模型生成的删除命令。

## 工具

| 工具 | 用途 |
|---|---|
| omd_get_capabilities | 平台能力、统计口径与授权模型 |
| omd_list_sources | 本地磁盘、总容量和可用容量 |
| omd_start_scan | 指定绝对 root_path 与 request_key，返回 scan_id/job_id |
| omd_list_scans | 保存的扫描摘要 |
| omd_get_scan | 根节点、完整性、过时状态和刷新引用 |
| omd_list_scan_issues | 无法访问或未遍历的位置 |
| omd_get_job | 后台作业进度、终态和结果引用 |
| omd_cancel_job | 请求协作取消，不回滚已完成的清理 |
| omd_list_children | 当前目录的子项分页 |
| omd_query_nodes | 名称、类型、大小、修改时间与范围查询 |
| omd_create_cleanup_plan | 指定 scan_id、node_ids、mode 与 request_key，后台生成计划 |
| omd_get_cleanup_plan | 规范化目标、hash、有效期和授权成员清单分页 |
| omd_execute_trash | 只执行 trash 计划，不能回退为永久删除 |
| omd_execute_permanent | 只执行单独批准的 permanent 计划 |
| omd_get_cleanup_result | 逐项结果、空间观测与刷新作业 |
| omd_release_scan | 释放没有有效引用的扫描结果，不删除源文件 |
| omd_reveal_node | 在系统文件管理器定位节点 |

查询使用服务返回的节点 ID。目录与其后代的大小相互包含，不能再次求和。分页默认 100 项，上限 500；next_cursor 绑定快照或固定结果集合及查询条件。更改筛选条件时从第一页重新开始。

## 扫描示例

```json
{
  "name": "omd_start_scan",
  "arguments": {
    "root_path": "/absolute/path/to/analyze",
    "request_key": "a-unique-key-for-this-request"
  }
}
```

取得 job_id 后轮询 get_job，直到 completed、completed_with_errors、cancelled、failed 或 interrupted。返回完整性和问题计数，不把无法访问目录当作零字节。

创建清理计划也返回后台作业。准备完成后读取 get_cleanup_plan，向用户展示服务端返回的目标、方式和有效期，再调用对应执行工具。

```json
{
  "name": "omd_execute_trash",
  "arguments": {
    "plan_id": "the-prepared-plan-id",
    "plan_hash": "the-returned-plan-hash",
    "request_key": "a-unique-key-for-this-execution"
  }
}
```

响应丢失时使用同一个 request_key 和完全相同的输入恢复；同键不同输入返回 request_conflict。已经受理的计划不会创建第二个执行作业。客户端断开不会自动取消后台扫描或清理。

## 错误与中断

清理前目标变化会跳过并报告 stale_plan。系统操作和数据库提交并非单一事务；崩溃窗口中的项目会标为 in_doubt，不自动重放，必须重新检查。中断清理涉及的扫描会保守标旧；正常清理后原根自动重新扫描，其余旧扫描明确提示需要重扫。

get_cleanup_result 在结果成员集合初始化完成前返回 result_not_ready。作业终止后可读取固定的部分结果，并通过 items_initialization_complete 判断清单是否完整。

macOS 回收站使用 NSFileManager，避免 Finder Automation 授权阻塞；Finder 可能不提供“放回原处”菜单，可以手动将项目移出废纸篓。Windows 使用专门的 Shell RecycleItem，接口、卷或命名空间不支持时返回错误，不调用通用永久删除作为回退。Windows 运行验证范围见 [平台说明](../crates/ohmydisk-platform/WINDOWS.md)。

## 集成测试

```sh
python3 tests/mcp_smoke.py --trash
```

脚本使用隔离的临时文件与数据目录，验证实际 stdio 握手、17 个工具、扫描、计划、两种临时文件清理、模式隔离、重连去重和刷新。可通过 OHMYDISK_BIN 指定待测可执行文件；否则优先查找 release，再查找 debug。
