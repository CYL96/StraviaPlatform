---
status: accepted
---

# 在 Generation Chain 之外拥有 Interaction Observation

> **部分被取代。** [ADR-0062](0062-persist-diagnostic-content-at-canonical-item-boundaries.md) 取代本 ADR 所链接设计 §5.1 的诊断文本按时间/大小封块边界；[ADR-0063](0063-record-four-direction-wire-debug-at-transport-boundaries.md) 取代本 ADR 的 Wire/canonical Debug payload 与凭据统一脱敏约束，并取代所链接设计 §6.2 的媒体外置、不可恢复与 omission 规则；[ADR-0077](0077-slim-interaction-observation-and-file-debug-manifests.md) 取代「数据库只保存 manifest 与索引」——Debug manifest 改为受管目录内的 `manifest.json`，索引改为进程内 `DebugTraceIndex`，同时删除 `debug_trace_manifests` 表与 `trace_manifest_updated` 事件。三项取代契约均已接受，当前实施契约以对应 ADR 为准。本 ADR 其余关于 Observation 所有权、Generation Chain 隔离、保留、清理与普通观测缺口不影响推理的决策仍为 accepted；存储失败适用下文的请求/任务局部处理约束；单实例承诺由 ADR-0077 原样延续。下文保留原始决策背景，不应把已取代条款视为当前目标。

Stravia 以 crate-private `InteractionObservation` 深模块拥有 Connect Client Interaction、Inference Run、Model Turn、Target attempt、Platform Tool、Delivery、Confirmed Upstream Usage 与 Debug Trace 的诊断投影。Inference Run 等执行模块只提交 typed observation event；Generation Chain 仍只保存完整交付的 `completed` / `incomplete` 模型历史，并仅向 Observation 提供已确认的 parent/root 关联。这样进行中、失败、取消、断线和拒绝请求可以实时展示并保留，而不会把可变运行状态塞进模型历史事实源。

普通事件和查询投影保存在 SQLite/PostgreSQL；大体积 Wire/canonical Debug payload 保存在 `data_dir` 下的受管分段文件，数据库只保存 manifest 与索引。Debug 在每个 Inference Run 准入时快照当前进程级开关，允许同一 Interaction 部分捕获；观察或抓包写入失败（包括容量耗尽）归所属诊断任务，保留 gap/partial，不得阻塞或改变推理结果。Admin 通过同一模块查询时间窗 forest、订阅带 cursor 的 SSE、清理历史并签发单次流式 ZIP 下载票据；Server 与 Desktop 不解释分组、Trace 或导出规则。

## 存储失败只影响所属请求或任务

本次用户决定直接替代本 ADR 此前「容量耗尽停止全实例」的规则。SQLite/PostgreSQL 数据库、事务日志或 `data_dir` 文件存储失败（包括容量耗尽）由实际操作所属请求或任务处理，实例继续运行；不保留旧规则的兼容分支。

- 关键同步存储操作沿已有 Result/错误路径失败当前相关请求或任务，不吞掉必要错误后报告成功；不广播全局 fatal，不设全入口故障 scope，不取消无关推理或后台执行。
- Observation/Debug 等独立异步写入失败归所属诊断任务，保留 observation_gap/partial，不改变 Inference Run 的响应、正常 retry/failover、Target 选择、Client Output Commit 或 Generation Chain；Observation 不成为执行事实源。
- Server/Desktop 在运行期错误后仍服务，无关请求可成功。关键初始化或升级失败仍使用原启动错误处理，不等于忽略必要存储操作。
- 已交付内容不可撤回，未收到或未持久化的用量保持真实未知/缺失边界，不补零、不估算、不自动重放模型调用。错误报告不以再次成功写入故障存储为前提。
- 独立长期 Stats Usage 保留其汇总、幂等、迁移和历史窗口契约；未证明应计事实可靠汇总时，相关破坏性清理必须明确失败，不先删详情后假报成功。普通诊断缺口不新增统计准入门槛。
- 队列拥塞、Trace 限额与普通 gap/partial 保留原语义；正常调度的有效缓存、stale/fallback 不变。不新增全局降级/只读模式、健康检查、替代错误传播层或自动重试。

实施状态：请求/任务局部错误处理已实现并通过真实宿主/HTTP 验收：Server SQLite/PostgreSQL 与 Desktop SQLite 在所属存储错误后继续服务；独立诊断保留 gap/partial，无关在途与后续请求、已交付后的后台任务不被取消；双库 Stats 写失败后相关清理返回 503 且应计事实保留。容量错误、普通 I/O、队列与 Trace 限额对照均沿各自错误边界处理。隔离测试与完整本地矩阵已通过；具体测试 seam 范围见[整体架构](../design/architecture.md#410-interaction-observation)，运行记录见[本地规格](../../.scratch/independent-usage-statistics/spec.md)。

## Considered options

- 扩展 Generation Chain 保存 running/failed/cancelled/debug：接口表面更少，但会破坏其“完整交付模型历史”的不变量，并让诊断丢失或清理影响历史语义。
- 在 Axum middleware 记录：容易拿到 client wire，却看不到 core 内 Hook、Model Turn、Target failover、Platform Tool、Client Projection 与 Desktop 共用执行，业务规则会泄漏到 transport adapter。
- 继续使用完成后 `request_logs` 加独立 debug JSONL：改动较小，但无法可靠形成 Interaction、实时状态、Run 子树、SSE cursor、统一保留和原子清理，两个记录路径会继续漂移。
- 把 Debug payload 全放数据库：备份简单，但最多 2 GiB 的 chunk/frame 会放大数据库与 WAL，并把主存储健康绑定到临时诊断负载。
- 全部使用文件：延续现有 Wire Capture，但时间窗查询、关联、状态更新和统计必须扫描文件，无法支撑管理面实时投影。

## Consequences

- 旧 `request_logs`、`LogStore`、`STRAVIA_WIRE_CAPTURE_DIR` 与 `/api/v1/logs` contract 干净删除；升级不迁移旧请求记录。
- Route Scheduling Strategy 继续通过 `UsageStatsStore` 读取 Model Turn/Target attempt Observation 的精确短期窗口；管理用量分析改读同一数据库内独立长期保存的 15 分钟 Stats Usage。源事实修订与汇总替换在同一事务提交，诊断详情清理只回收相应工作检查点，不撤销已发生的汇总；不新增永久逐请求计量档案。具体指标、升级回填和绝对时间查询契约见所链接设计。
- Observation 与 Debug Trace 跟随同一保留期；清理用 tombstone 和启动回收保证数据库索引与本地分段文件最终一致。
- 普通 Observation 与 Debug Trace 均只遮盖实际 HTTP `Authorization` header 值（名称不区分大小写），普通正文和工具参数中的同名字段原样保留。普通记录的实时通道、落盘和导出可能保留下载授权及其他凭据，应按敏感诊断处理；这不扩大普通捕获范围，不改变保留期、权限、进程日志、普通客户端错误或模型侧凭据保护，也不回写旧记录或自动续签授权。
- Debug Trace 只保存四方向应用协议传输边界的原始内容，不是 packet capture；只在入队前替换 HTTP `Authorization` header 值，其他 header、URL、body、message 与媒体原样保留。Canonical Item 内容属于普通 Observation，不复制为 Debug checkpoint。
- 当前只承诺单 Gateway 实例实时行为。多实例通知、共享 Debug payload storage 和集群级易失开关需要另行决策，不能由 PostgreSQL 共享表暗示已经支持。
- 完整接口、状态、存储、Admin contract、画布和验收场景见 [`docs/design/interaction-observation.md`](../design/interaction-observation.md)。
