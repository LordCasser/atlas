## Why

`domain_rules(action="delete")` 与 `fp_dispatches(action="delete")` 会立即修改活动项目数据库中的持久化 overlay 数据；后者还会重建物化边并刷新内存图。当前一次工具调用即可执行删除，现代交互客户端没有机会在用户看清精确目标后阻止模型误操作。

MCP 2026-07-28 的 form elicitation 与 MRTR 已能提供一次明确确认。删除确认还必须绑定初始项目和原始参数：若两轮之间切换项目或改写目标，确认不能落到另一个对象。为此，本变更使用有界、短时、一次性的服务端 `requestState` 句柄，而不是信任客户端回显的目标数据。

## What Changes

- 对有效 MCP `2026-07-28+` 且声明表单 elicitation 的客户端，合法形状的 `domain_rules(delete)` 与 `fp_dispatches(delete)` 初始调用返回一个 required boolean 确认表单，不先执行核心删除。
- 初始轮次固定当前 `ActiveProject`、原工具名和完整参数，并生成不可预测的 opaque `requestState`；状态最多保留 300 秒、总量有界，第一次重试时原子消费。
- `accept` 且 `confirm: true` 只有在工具名与完整参数仍精确匹配时，才在固定项目上调用一次现有核心删除路径；项目切换不得改变删除目标。
- `decline`、`cancel` 或 `accept` 且 `confirm: false` 返回完整非错误的未删除结果，不调用核心处理器。缺失、过期、重放、错误流程、改写参数或畸形响应返回标准 `invalid_params`。
- 旧协议、无表单能力、无活动项目、非 delete action 和缺少合法目标的调用保持现有直接核心语义；确认仅是误操作防护，不是授权、身份或 RBAC 决策。
- 不改变 15 项工具目录/schema、overlay 数据模型、删除事务、边物化、图刷新、直接 `ToolRouter::call_tool()` 或现有 MRTR/Tasks 流程。

## Capabilities

### New Capabilities

- `mcp-overlay-delete-confirmation`: 定义两个持久化 overlay 删除操作的能力门控确认表单、固定项目状态绑定、一次性兑换和兼容回退语义。

### Modified Capabilities

<!-- 无。 -->

## Impact

- 影响 `crates/atlas-mcp/src/lib.rs` 的 rmcp 调用前适配、确认状态生命周期和 wire/并发测试。
- 可能为 `crates/atlas-mcp/Cargo.toml` 增加已有锁文件版本的 UUID v4 直接依赖，用于生成不可预测 opaque handle；不升级 rmcp。
- 复用 `crates/atlas-mcp/src/tools/mod.rs` 的固定项目 router；核心 `domain_rules` / `fp_dispatches` 删除实现保持不变。
- 更新路线图与变更日志，明确现代交互确认和 legacy/no-form 兼容边界。
