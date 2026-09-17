## Why

`domain_rules` 与 `fp_dispatches` 的工具描述已经说明不同 `action` 需要不同参数，但当前 `inputSchema` 只列出属性，没有机器可读的动作级必填条件。客户端因此会把缺少 `rule_id`、`field_qname` 等明显无效的调用发送到服务端，直到 handler 返回字符串错误才发现问题。

Atlas 已建立完整 JSON Schema object 的无损 transport 边界，rmcp 3.0.1 也会原样传递根级 `allOf`、`if`、`then` 与 `anyOf`。现在可以在不改变 handler、工具数量或协议控制面的前提下补齐这两个 mutation/read 复合工具的公开输入契约。

## What Changes

- 为 `domain_rules` 增加动作级条件：`add` 要求 `rule_kind` 与 `pattern`，`delete` 要求 `rule_id`。
- 为 `fp_dispatches` 增加动作级条件：`add` 要求 `field_qname` 与 `target_qname`，`delete` 要求 `annotation_id` 或 `field_qname` 至少一个。
- 条件判断显式要求 `action` 字段存在，保持省略 `action` 时默认执行 `list` 的现有行为。
- 保留 handler 的运行时校验作为服务端最终防线，并增加 Atlas 注册 schema 与 rmcp transport 的条件约束回归。
- 不改变 15 项工具目录、工具顺序、属性集合、顶层 unconditional `required`、MRTR 删除确认、overlay 持久化或 handler 响应格式。

## Capabilities

### New Capabilities

- `mcp-action-aware-tool-schemas`: 定义复合 MCP 工具按 `action` 暴露机器可读条件必填参数，同时保持默认读操作和运行时防御兼容。

### Modified Capabilities

- `mcp-tool-schema-fidelity`: 现有无损 schema transport 需要继续保证新增根级条件关键字从 Atlas 注册到 rmcp wire 不被投影或改写。

## Impact

- 影响 `crates/atlas-mcp/src/tools/tool_schemas.rs` 的两个工具 schema，以及 schema/transport 定向回归。
- 对会预先校验 JSON Schema 的客户端，这是有意的契约收紧：此前必然被 handler 拒绝的缺参调用会更早失败；合法调用与省略 `action` 的默认 list 不变。
- 不新增依赖，不改变数据库 schema、Focus、Tasks、MRTR state 或其他工具运行语义。
