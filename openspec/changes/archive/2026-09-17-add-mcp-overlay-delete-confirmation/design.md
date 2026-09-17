## Context

Atlas 的两个公共工具会删除持久化 overlay：

- `domain_rules(action="delete", rule_id=...)` 直接删除生命周期规则；
- `fp_dispatches(action="delete", annotation_id=... | field_qname=...)` 删除函数指针映射，并随后清理物化边、刷新图。

两者都由 `OverlayMutation` 核心契约执行，但 rmcp 适配层当前只对缺失项目路径和 `explore` 候选做 MRTR。现代 form-capable 客户端无法在删除前展示确认。

确认不是授权。旧协议或非交互客户端仍可直接调用删除；服务器也没有认证主体或 RBAC。本切片只降低模型/用户误操作风险。与现有非破坏性无状态 MRTR 不同，删除确认需要跨轮固定项目和目标，否则标准客户端在两轮之间发生 `project(open)` 时也可能把同一个 ID 作用到不同项目。

## Goals / Non-Goals

**Goals:**

- 只确认两个已知持久化 delete action，并展示其精确目标和固定项目。
- 使用现代协议 + 显式 form capability 门控，保持 legacy/no-form 行为。
- 将确认 offer 绑定到完整原参数和固定 `ActiveProject`，短时、一次性、并发安全地兑换。
- 接受后只调用现有核心一次；拒绝/取消不进入核心。
- 保持工具目录、schema、数据模型与直接路由结果不变。

**Non-Goals:**

- 不建立通用 mutation policy、审批工作流、认证、授权或 RBAC。
- 不确认 add/learn/project open、task cancel 或其他副作用。
- 不把 confirmation 解释为恶意客户端不可绕过的安全边界。
- 不持久化 confirmation，不跨服务重启恢复，也不推进 stateless HTTP。
- 不顺带实现 include-context、task push 或新的 transport。

## Decisions

### 1. 同时覆盖两个 overlay delete，但不泛化所有 mutation

适配层只识别以下固定调用形状：

- `domain_rules` + `action="delete"` + 非空字符串 `rule_id`；
- `fp_dispatches` + `action="delete"` + 非空字符串 `annotation_id`，否则使用非空字符串 `field_qname`，与现有核心优先级一致。

只有活动项目存在、协议日期有效且不早于 `2026-07-28`、客户端声明 `elicitation.form`、且本轮没有 MRTR 响应/state 时才发确认。缺失目标、错误类型、非 delete action 或未打开项目继续进入现有核心，让既有校验产生权威结果。

两个操作属于同一持久化 overlay 删除边界，统一确认可避免用户看到不一致保护；本切片不根据 `ToolContract` 自动拦截未来 mutation，新增操作必须另行设计。

### 2. 表单用 action 与 required boolean 双重表达确认

唯一 input request 使用固定 input id `confirm_overlay_delete`。表单包含 required boolean `confirm`，标题和描述明确该操作会修改当前项目持久化 Atlas 数据，默认值为 `false`。message 展示 canonical 项目路径、工具类别和精确 rule/annotation/field 标识。

响应处理：

- `accept` 且 `confirm == true`：尝试兑换状态并执行删除；
- `accept` 且 `confirm == false`、`decline` 或 `cancel`：兑换状态后返回 `status: "not_deleted"` 的完整非错误结果；
- 未知 action、缺失/非布尔 content、额外 input response：`invalid_params`。

表单 schema 只改善客户端 UI，服务器不得依赖客户端执行 schema validation。

### 3. requestState 是服务端 opaque handle，不承载可信 JSON

初始轮次生成 UUID v4 handle，并在服务内存中保存：

- flow kind/version；
- 原工具名与完整 `serde_json::Value` 参数；
- 固定 `ToolRouter`，其内部持有初始 `Arc<ActiveProject>`；
- 用户可见目标描述；
- 创建时间。

handle 本身不包含路径或参数，不解析客户端提供的结构化 state。随机性使客户端无法构造另一个有效确认；可信内容始终留在服务端。这符合 MCP/rmcp 对 `requestState` 的另一种安全用法：把它仅作为 opaque server-side handle，而不是信任回显 payload。

状态 TTL 为 300 秒。每次签发和兑换都清理过期项；未过期 pending 数量硬上限为 128，达到上限时返回适配器错误，不执行删除。服务关闭自然清除全部状态。

### 4. 第一次兑换原子消费，失败也不恢复

兑换在互斥锁内按顺序执行：清理过期项、按 handle `remove`、释放锁，再校验工具名、完整参数和响应。不存在、过期或已兑换的 handle 统一返回 `invalid_params`。

状态一旦被取出即消费，包括参数改写、错误工具、畸形响应和核心删除失败。这样同一 offer 在并发或网络重放下最多触发一次核心尝试。客户端如需重试，必须重新发起一个初始 delete 并获得新确认。

完整参数使用 `serde_json::Value` 结构相等比较，区分字段缺失与显式 `null`；接受后执行保存的原参数，而不是客户端回显副本。

### 5. 接受后使用固定项目 router，拒绝后直接完成

`PreparedToolCall` 可携带可选 execution router override。接受确认后适配层把保存的固定 router 交给现有 blocking gate 与 `ToolRouter::call_tool()`，因此两轮之间外层 `project(open)` 不会迁移删除目标。

拒绝、取消或显式 false 构造一个普通成功 `CallToolResult`，包含 `ok: false`、`status: "not_deleted"`、`reason: "confirmation_declined"`、原工具和目标描述；不调用核心、不重新发 confirmation。

核心返回的 not-found、数据库错误、FP edge materialization 或 graph refresh 错误保持现有 `isError` 与文本格式。确认层不复制这些规则。

### 6. 流程隔离与兼容边界

确认重试必须同时携带唯一固定 input response 与非空 `requestState`。项目路径和候选选择仍拒绝 state；其他没有 MRTR 流程的工具若收到 input responses/state，返回 `invalid_params`，不得静默执行核心。

旧协议、无 form capability 或没有活动项目时不签发状态，直接执行既有 delete。该兼容行为意味着 confirmation 不是 authorization；OpenSpec、路线图和变更日志必须明确这一点。

## Risks / Trade-offs

- [服务端状态使 stdio adapter 不再完全无状态] → 只保存最多 128 个、300 秒、不可持久化的一次性确认，并在服务关闭时释放；不改变 query/task 状态模型。
- [UUID handle 被猜测] → UUID v4 提供足够不可预测性；状态还绑定完整工具/参数和固定项目，猜中后首次错误兑换也会消费。
- [项目切换发生在两轮之间] → 保存固定 router，接受后始终作用于原项目；用户消息明确显示该项目路径。
- [核心删除失败后 token 已消费] → 这是防重放优先的保守策略；用户可重新发起确认，不允许同一 token 再次触发副作用。
- [legacy/no-form 仍直接删除] → 为兼容性有意保留，因此不得把该功能描述为权限控制。
- [pending map 被填满] → 硬上限阻止内存无界增长；满载错误不会执行删除。

## Migration Plan

1. 增加确认状态容器、固定项目 capture、目标识别和纯单元测试。
2. 接入 `prepare_tool_call()` 与 execution router override，覆盖两个真实核心删除、项目切换、并发重放和拒绝分支。
3. 更新文档，运行严格 OpenSpec、MCP/CLI 全量测试和 Clippy，完成独立审查。
4. 归档后全局严格校验并 `cargo clean`。回滚只需移除适配层确认分支和临时状态；数据库与工具 schema 无需迁移。
