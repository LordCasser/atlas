## Context

`ScopedSearchService::execute()` 在一次调用内先统计 indexed files 与 `file_inventory`，同步物化一个有界子集，再返回结果、coverage 与 `deferred_file_ids`。MCP `handle_search_sync()` 随后通过 `enqueue_background_file_focus()` 为 deferred files 建立 `FocusResult`；`AnalysisEnvelope` 只有在 pending count 大于零时写入 `analysis.retry_after_ms`。

后台 scheduler 与重放线程并发写读同一个 Store。计数、搜索和入队不是一个 SQLite 快照，也不应通过长事务或全局锁强行串行化。已观察到两种合法时序：

1. 重放统计到 2/3 files，后台随后提交第 3 个；本轮结果仍只有 2 个，但入队时已无 pending。
2. 重放统计到 2/3 files，后台在本轮 `run_search()` 前提交；结果已有 3 个，但 stale coverage 标记仍为 partial，入队时同样无 pending。

两者都会得到无 retry marker 的 terminal partial，因此属于产品级 TOCTOU，而非只需延长测试 sleep 的 flake。

## Goals / Non-Goals

**Goals:**

- 当 deferred work 在 execute→enqueue 边界内已经完成时，重新读取一次当前事实，使 terminal coverage 与 Store 一致。
- 保持 pending、失败、预算耗尽和 inventory discovery 不完整的既有语义。
- 为该竞态建立不依赖线程调度概率的可判别回归。

**Non-Goals:**

- 不把整个 scoped search 包进数据库事务或阻塞后台 scheduler。
- 不提高同步 cold-search 的两文件上限，不扩大 Focus closure 或搜索结果 limit。
- 不改变 `ScopedSearchService` 公共 API、Tasks 控制面、query snapshot TTL 或搜索 schema。
- 不把所有 partial 自动升级为 complete；只有一次重读的当前事实能证明时才升级。

## Decisions

### 1. 以“partial + deferred + 无 pending”作为单次协调触发器

第一次执行后仍按现有方式尝试入队 deferred files。只有同时满足以下条件才重读：

- coverage 为 partial；
- `deferred_file_ids` 非空，说明 partial 与可物化文件直接相关；
- 成功入队后返回的 FocusResult 没有 pending work，或成功返回 `None`，说明此刻没有可等待的共享作业。

入队错误与“无工作”严格区分并向现有 search 错误路径传播，不作为完成证据。上述组合精确表示“执行看到旧状态，但调度边界已看到终态”的可疑窗口。普通 partial、真实 pending 以及无 deferred 的永久边界都不额外执行。

### 2. 重读最多一次，并完全复用现有搜索服务

协调不修改第一次响应字段，也不靠命中数量猜测完整性，而是用同一个 `ScopedSearchService` 和同一份 `ScopedSearchRequest` 再执行一次。第二次结果重新经过现有 scope/inventory/structural coverage 判定和排序。随后再对第二次 deferred files 调用一次现有入队函数。

最终响应使用与“是否重读”决策同一次 tracker 读取所得的 pending 数量和 ETA。若作业在该读取后立即完成，本轮最多多发一个安全 retry；不得重新读取 tracker 后把旧 partial 降为无 retry 的终态。

不循环重读。若第二次仍 partial：

- 有 pending 时保留 retry ticket；
- 无 pending 时保留 terminal gap，表示现有核心已判定为真实边界或失败，而不是无限自旋。

### 3. 回归直接构造竞态后的稳定状态

测试先通过真实 `ScopedSearchService` 得到 partial + deferred，再同步物化 deferred 文件，模拟“后台恰在 execute 后完成”。随后调用协调 helper：第一次入队必须观察到无 pending，单次重读必须得到 complete 和全部命中。另以删除 deferred 文件构造 terminal no-progress，验证一次规范化 retry 后稳定为 terminal partial；并验证作业在 pending 观察后完成时，本轮仍保留该一致 retry 观察。现有端到端恢复测试继续覆盖最终 MCP JSON、ready snapshot、无 stale gap/warning 和完整命中。

### 4. 不用 coverage 字段反向控制授权或缓存

该变更只修复查询完成度表达。`coverage.state`、gap 和 retry marker 不是权限判断；15 项工具目录、输入 schema、查询参数、结果排序和缓存权威均不改变。

## Risks / Trade-offs

- [竞态时多执行一次 scoped search] → 仅在 partial + deferred + zero-pending 的窄窗口触发，且最多一次；文件已物化时主要是 Store 读取。
- [第二次执行可能发现新的 deferred work] → 重新使用现有入队和 retry 规则，不隐藏新 pending。
- [失败作业 pending count 为零] → 第二次执行仍无法证明 full 时保持 terminal partial；不把失败误报为 complete，也不形成循环。
- [测试依赖内部 helper] → helper 承载真实 execute→enqueue 协调边界，测试仍使用真实 Store、inventory、structural materializer 和搜索服务，而非伪造 coverage JSON。

## Migration Plan

1. 提取单次搜索执行与 deferred Focus 协调 helper。
2. 增加确定性“execute 后 deferred 已完成”回归，并保留现有恢复循环测试。
3. 串行运行严格 OpenSpec、`atlas-mcp`、MCP CLI 与 Clippy 验收。
4. 独立审查后归档；如需回滚，只移除单次协调 helper，不涉及数据迁移。
