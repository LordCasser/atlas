# 分析响应信封重构 — 修订记录

> 本文件为历史参考，记录修复链路与设计决策的上下文。权威约束见 `docs/architecture.md`。

## 问题根因

MCP 工具反复返回 `wait_then_resume` 的根因链条：

| 环节 | 文件 | 问题 |
|------|------|------|
| ① | `runtime.rs` | `precision` 硬编码为 `Boundary + Medium`，永不为 `Certain` |
| ② | `runtime.rs` | `pending_closure_ids` 永不为空（至少含前台 + 后台闭包 ID） |
| ③ | `runtime.rs` | 前台闭包 ID（已完成）污染 `pending_closure_ids` |
| ④ | `scheduler.rs` | `process_detached_job` 完成后无回调/通知机制，MCP 层无法感知终态 |

**真根因：环节 ④** — 后台任务完成事件被丢弃。

## 修复链路

| 修复 | 效果 |
|------|------|
| 新建 `JobTracker`（共享完成追踪 + ETA） | 环节 ④ |
| `scheduler` 完成时调 `mark_done + record_elapsed` | 环节 ④ |
| `precision` 从 `coverage_counts` 实际推导 | 环节 ① |
| 前台闭包 `mark_done` immediately，`pending` 排除前台 ID | 环节 ③ |
| `tracker.are_all_done(&pending)` 终态判定 | 终态收敛 |
| `AnalysisEnvelope` 重构为 `retry_after_ms` + `gaps`，删除 `partial_result`/`state`/`next_action` | 信号精简 |
| `GapRecord {scope, reason, detail}` 替代 flatten 字符串 | 可消费信号 |
