## Why

冷启动 `search` 会先同步物化至多两个文件，再把其余 inventory 候选交给后台 Focus。`resume_query` 与后台提交可以并发：重放可能在读取“仍缺一个文件”的计数后开始，而后台随后完成；本轮搜索已经看到全部命中，调度器也确认没有待处理作业，但早先计算的 `inventory_backed` / bounded 标记仍会把响应定格为无重试提示的 `coverage.state="partial"`。

这不是单纯测试等待不足。客户端可以在 Focus 即将完成时合法恢复查询，当前竞态会把已经收敛的结果发布成永久 gap。验收中已在单线程串行环境复现，包括 `total=3` 已完整但 coverage 仍 partial 的结果。

## What Changes

- 当 scoped search 返回 partial、携带 deferred file IDs，而后台 Focus 入队确认这些文件已无待处理工作时，MCP search 适配层立即复用同一请求做至多一次当前事实重读。
- 重读若证明 scope 已具备完整 structural coverage，则返回 `coverage.state="complete"`、完整结果且不保留过时的 bounded/gap/retry 语义。
- 重读后若仍有真实待处理作业，则继续生成现有 `query_id` + `analysis.retry_after_ms`；若只剩真实预算、发现或失败边界，则保留现有 terminal partial gap。
- 不增加轮询次数上限之外的循环，不改变 Focus scheduler、文件物化、搜索排序、工具 schema、15 项目录、Tasks 或其他查询流程。

## Capabilities

### New Capabilities

- `mcp-focus-search-convergence`: 定义 cold scoped search 在后台完成竞态下的单次终态协调与 coverage/retry 一致性。

### Modified Capabilities

<!-- 无。 -->

## Impact

- 影响 `crates/atlas-mcp/src/tools/search.rs` 的搜索执行与后台 Focus 协调、`crates/atlas-mcp/src/tools/mod.rs` 的可失败入队边界，以及定向竞态回归。
- 不改变 `atlas-engine::ScopedSearchService` 的搜索算法、SQLite schema 或公共请求/响应类型。
- 需要串行运行 MCP/CLI 回归并继续使用 `CARGO_BUILD_JOBS=1`，避免高峰内存导致 OOM。
