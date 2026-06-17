# 分析响应信封 v2 重构 — 修订记录

> **状态**: 已完成 (2026-06-17) — 全部约束已合并至 `docs/architecture.md` §1/§10
>
> 本文件仅保留问题根因与修复链路作为历史参考。权威约束请参阅 `architecture.md`。

## 问题根因（4 环链条）

| 环节 | 文件 | 问题 |
|------|------|------|
| ① | `runtime.rs` | `precision` 硬编码为 `Boundary + Medium`，永不为 `Certain` |
| ② | `runtime.rs` | `pending_closure_ids` 以 `vec![closure_id.clone()]` 初始化，至少 2 项，永不为空 |
| ③ | `runtime.rs` | 前台闭包 ID（已完成）被加入 `pending_closure_ids`，污染待处理列表 |
| ④ | `scheduler.rs` | `process_detached_job` 完成后仅返回 `Ok(())`，无回调/共享状态/通知机制 |

**真根因：环节 ④** — 后台任务完成事件被丢弃，MCP 层无法区分"仍在运行"与"已完成"。

## 修复链路

| 修复 | 位置 | 效果 |
|------|------|------|
| 新建 JobTracker | `engine/focus/job_tracker.rs` | 共享的 job 完成追踪器，含 ETA 计算 |
| scheduler 完成通知 | `scheduler.rs` | `process_detached_job` 完成时调 `mark_done + record_elapsed` |
| precision 实际推导 | `runtime.rs` | `total_coverage > 0` → `ClosureComplete+High` |
| pending 排除前台 ID | `runtime.rs` | `mark_done` immediately, pending 置空 |
| 终态判定 | `mod.rs` | `tracker.are_all_done(&pending)` |
| 响应信封 v2 | `analysis_envelope.rs` | 删除 `partial_result`/`background_refinement`/`state`/`next_action`；仅保留 `retry_after_ms` + `gaps` |
| lifecycle/branch_diff gaps 结构化 | `lifecycle.rs` / `branch_diff.rs` | GapRecord `{scope, reason, detail}` 替代 flatten 字符串 |

## 响应三态终局

```
状态 1 — 非终态（后台仍在运行）
{ "result": {...}, "analysis": {"retry_after_ms": <eta>} }

状态 2 — 终态：完整
{ "result": {...} }

状态 3 — 终态：有永久缺口
{ "result": {...}, "gaps": [{"scope":"...", "reason":"...", "detail":"..."}] }
```

Agent 消费逻辑：`resp.analysis?.retry_after_ms` → poll / `resp.gaps` → use_with_caution / 否则 → use_with_confidence。

## 实体变更总览

- **新增 (+1)**: `JobTracker` (`engine/focus/job_tracker.rs`)
- **修改 (Δ7)**: `scheduler.rs`, `runtime.rs`, `mod.rs`, `analysis_envelope.rs`, `lifecycle.rs`, `branch_diff.rs`, `graph.rs`
- **删除 (−5)**: `BackgroundRefinement` 结构体, `partial_result` 字段, `state`/`confidence`/`next_action` 公共暴露

测试基线: `atlas-engine` 370 pass, `atlas-mcp` 344 pass, 0 failures.
