# OhMyDisk MCP 接入

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
command = 'C:\Program Files\OhMyDisk\ohmydisk.exe'
args = ["mcp", "--stdio"]
```

应用构建和测试不会自动修改现有 Codex 配置。[OpenAI 官方接入说明](https://learn.chatgpt.com/docs/extend/mcp)

## 受限 Windows 宿主

某些 agent 宿主及 GitHub 托管 runner 不允许子进程脱离 Job Object。自动服务启动会明确返回 `Access is denied`，不静默改变后台服务的寿命约定。

在独立终端显式启动前台服务，然后使用上面的 MCP 配置连接：

```powershell
& 'C:\path\to\ohmydisk.exe' service --stay-alive
```

保持这个终端运行。前台模式不会因空闲自动退出，关闭终端会结束该服务；默认自动启动的后台服务仍会在空闲后退出。服务与 MCP 必须使用同一个用户及数据目录；自定义目录时，两边都传入相同的 `--data-dir`。

## 授权约定

采用 host_confirmed：agent 宿主负责向用户展示精确清理计划，并在分发执行调用前获得批准。OhMyDisk 不再弹出第二次确认。请在宿主中分别为 omd_execute_trash 和 omd_execute_permanent 设置人工审批，永久删除必须单独批准；不要对整个服务无条件自动批准。

工具的破坏性 annotations 只描述行为，不能证明宿主真的询问过用户。文件名、工具返回内容和 clientInfo 也不是用户授权。计划由服务端生成，执行只接受计划 ID、hash 和请求键，不接受任意新路径、脚本或模型生成的删除命令。

## 工具

扫描任务的 `omd_get_job` 响应，以及扫描详情和历史摘要，会返回可选的
`elapsed_ms`（毫秒）。扫描中它包含排队、遍历、保存和建立索引的时间；任务进入
完成、取消或失败状态后冻结并保存，重启服务后仍可读取。摘要的 `started_at` 和
`finished_at` 是时间戳，原有 `created_at` 仍表示快照时间。旧记录或异常退出前
没有保存总耗时的记录可能缺少这些值，agent 应按未知处理，不从快照时间推算。

| 工具 | 用途 |
|---|---|
| omd_get_capabilities | 平台能力、统计口径与授权模型 |
| omd_list_sources | 本地扫描入口（挂载点）、卷容量和可用容量 |
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

脚本使用隔离的临时文件与数据目录，验证实际 stdio 握手、17 个工具、扫描、计划、两种临时文件清理、模式隔离、重连去重和刷新。它还验证 attributed 排序、回包口径、未知项与数值过滤、跨口径 cursor 拒绝，以及可对账时的空间等式。临时文件系统支持硬链接时，用一组硬链接检查“未测量”不会被当成已测零字节；不要求每个平台都提供 private size。可通过 OHMYDISK_BIN 指定待测可执行文件；否则优先查找 release，再查找 debug。

受限 CI 使用 `--service-mode supervised`，由测试监督独立的临时前台服务进程，完整执行相同的扫描、清理、重连及幂等断言，再关闭自己的服务并清理临时文件。默认 `auto` 模式用于验证允许自动脱离的宿主；监督模式通过不代表宿主已经允许 Job Object breakaway。


## 大小统计与容量对应

GUI 默认展示已确认文件占用。MCP 保留原查询默认值：未传 `size_metric` 时仍按逻辑大小排序和筛选。`get_capabilities` 返回 `application_version`、`size_metrics: ["logical", "allocated", "attributed"]`、`default_display_metric: "attributed"` 和 `legacy_query_default: "logical"`。应用版本和 `api_version` 分别表示程序版本与接口契约版本。

`list_children` 和 `query_nodes` 都接受以下口径，回包的 `size_metric` 表示实际采用的口径：

| `size_metric` | 数值来源 | 使用限制 |
|---|---|---|
| `logical` | 节点的 `logical_bytes`，按文件目录项累计逻辑长度 | 旧接口默认值；稀疏文件、压缩、硬链接和共享区块可能使总量超过容量 |
| `allocated` | 节点的 `allocated_bytes` | 文件系统报告的分配大小，可能为 `null`；APFS clone 共享的块不能据此当作独占空间 |
| `attributed` | `space_usage.attributed_bytes`，仅累加已经测量的贡献 | 必须同时读取测量状态；未知值不回退到逻辑大小 |

macOS 的 `mac_private_size` 使用实际返回的 `ATTR_CMNEXT_PRIVATESIZE`。Windows 的 `ntfs_unshared_allocation` 仅用于本地、不支持共享簇的 NTFS；普通非 reparse、单链接文件复用原生分配大小，压缩或稀疏文件使用相应的原生分配值。多链接文件、没有返回 private size 的项目、ReFS、网络卷及无法识别的文件系统保留为未测量。扫描对同一文件系统内已经遍历的目录身份去重；重复目录入口保留边界节点，不再次遍历或累计内容。普通硬链接仍按目录项计逻辑大小。

这些值都不能作为清理收益承诺。应在清理后重新读取系统空间观测，检查可用容量如何变化。

### 查询已确认占用

```json
{
  "name": "omd_query_nodes",
  "arguments": {
    "scan_id": "the-saved-scan-id",
    "scope_node_id": "the-returned-node-id",
    "descendants": true,
    "size_metric": "attributed",
    "limit": 100
  }
}
```

结果按该口径从大到小排序，同值按节点 ID 排序，完全未测量的项目排在后面。节点的原始 `logical_bytes`、`allocated_bytes` 与路径保留原义，`space_usage` 另含以下字段：

| 字段 | 含义 |
|---|---|
| `attributed_bytes` | 已测量部分的字节之和，目录含子项贡献 |
| `measured_entries` | 已测量的项目数；返回零字节的文件也算已测量 |
| `unmeasured_entries` | 未测量项目或未枚举子树的标记数，是未知范围的下界，不能当作准确的漏扫文件数 |

`measured_entries=0` 且 `unmeasured_entries>0` 表示没有可用的数值测量，不能把存储中的 `attributed_bytes=0` 展示为“已测零字节”。部分测量的目录可以展示已知贡献，但必须保留未测量状态；共享文件贡献为零也不表示文件或目录为空。

没有数值筛选时，结果保留这些项目。传入 `min_bytes` 或 `max_bytes` 后，仅完整测量的值参与比较，回包的 `excluded_unmeasured` 给出其余符合名称、类型和时间条件、但因大小未知而排除的项目数。修改 `size_metric` 或筛选条件后从第一页重查；旧 cursor 不能用于新口径。

### 读取容量账本

`get_scan.space_accounting` 保存本次扫描前后的原生观测及归属结果，`list_scans` 的扫描摘要也包含该字段。它不会随着当前磁盘空间变化而改写。旧的 `get_scan.logical_bytes` 和 `size_metric: "logical_bytes"` 仍描述逻辑总量，不代表 GUI 默认显示的占用。

当 `reconciliation_status="balanced"` 时，以扫描后的同口径观测构图：

```text
C = A + E + R
C：系统报告的文件系统可见容量
A：原始可用空间
E：本次扫描范围内已确认的文件占用
R：共享、系统及未归属空间
```

例如容量为 995 GB、原始可用 245 GB、已确认文件占用 500 GB，剩余区域为 250 GB。即使原始逻辑总量为 1.1 TB，容量图仍以 995 GB 为边界；程序不截断逻辑大小，也不把未知空间按比例分给目录。

`R` 还可能包括范围外文件、权限未覆盖的子树、快照、文件系统元数据、其他共享卷和系统保留，不能全称为共享块或垃圾。`C-A` 表示该入口下的已用及系统保留空间，不能宣称是本卷独占占用。

观测对象包含 `volume_id`、`total_bytes`、`available_bytes`、`captured_at`、`source`、`capacity_scope` 和 `provider`；时间为 Unix 毫秒。原生 `used_bytes` 与 `caller_available_bytes` 是可选详情。macOS 使用原始 SIZE/SPACEAVAIL 或同口径 statfs，Windows 使用 actual-volume 计数；不会将预计可清理容量、当前用户配额和整个卷空闲混入同一等式。Windows 旧接口回退的 `caller_view` 只提供明确标记的容量概览，不参与归属加法图。

| 状态字段 | 取值与处理 |
|---|---|
| `measurement_status` | `complete`、`partial` 或 `unavailable`；描述文件贡献的测量覆盖情况 |
| `reconciliation_status` | `balanced` 时可以验证等式；`inconsistent` 表示观测范围、容量或数值不能对应，`unavailable` 表示缺少扫描后观测 |
| `shared_unattributed_bytes` | 只有可以对应时才有数值；失败时为 `null`，不能替换成零 |

文件测量为 `partial` 或 `unavailable` 时，容量账本仍可能 `balanced`：余量包含尚未归属的内容。若 E 超出 C-A，或前后观测无法对应，客户端应展示 OS 容量概览及明确状态，停止绘制加法归属图，保留原始节点数据。

旧 JSON 和压缩快照可以继续读取。没有新测量字段的快照仍支持原逻辑查询；显式请求 attributed 会返回 `measurement_required`，GUI 会提示重扫。`list_sources` 中没有 `space_observation` 的旧兼容容量字段只能用于详情，不能充当一次成功的原生容量观测。

新版 MCP 适配器可以连接已经运行的旧后台。明确请求 `allocated` 或 `attributed` 时，适配器要求后台回包确认同一 `size_metric`；旧后台忽略新参数或返回其他口径时，工具返回 `backend_upgrade_required`，不会把逻辑查询当作成功的占用查询。省略口径或明确请求 `logical` 仍保持旧兼容。应用不会为此停止正在运行的后台或作业；可在原作业结束后更新并重启后台。

### macOS 扫描入口

`list_sources` 的每项代表一个挂载入口，不能将所有项的容量简单相加。macOS 的 `/` 与 `/System/Volumes/Data` 提供不同入口，系统与数据卷共享 APFS 容器空间。它们的文件系统命名空间标识 `id` 可以相同，通过 `mount_path` 区分入口，启动扫描时传入该路径。文件系统可见容量也不等同于整块物理 SSD 的硬件容量。

### 并发与取消

`get_capabilities.scan_concurrency` 返回当前目录 worker、同服务文件系统预算和较重查询的上限。Tokio 协调固定数量的目录 worker，原生阻塞调用在 blocking pool 中执行；取消停止新派发，关闭队列并等待已启动的 worker 结束后才发布终态。扫描进度中的字节计数仍是逻辑长度，不能当作占用或在未知总数时计算百分比。

同一快照的冷读取合并为一次解码和索引构建，已缓存的其他快照仍可查询。快照、索引和容量账本都保留扫描时的状态；执行清理后的刷新会生成新的快照。


`omd_start_scan` 可选 `workers`（1–8）控制目录任务上限。默认 4，多个扫描共享
全局 8 个文件系统许可；不会按文件创建任务。更多 worker 不保证所有目录结构都更快，
单个宽目录、深链目录和元数据热缓存场景可以选择较小值。作业结果的 `scan_workers`
记录实际配置上限，活跃任务数量仍由目录树及全局预算决定。


查询响应就地返回 `size_semantics`、`measurement_status`、`reconciliation_status`
和 `attribution_provider`，不应仅凭一个 attributed 数字宣称当前容量已对账。
`get_job.progress.bytes` 保留旧版逻辑累计语义；占用空间应读取
`get_scan.space_accounting`，界面扫描状态仅使用项目计数、阶段和耗时。
