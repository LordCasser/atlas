## Why

Atlas 当前把工具输入 schema 拆成 `type`、`properties` 与 `required` 三个固定字段，再由 rmcp 适配层重新拼装根对象。现有嵌套约束尚能工作，但任何新增在根级的 JSON Schema 2020-12 关键字（如 `oneOf`、`if`/`then`/`else`、`$defs` 或 `additionalProperties`）都会在 transport 边界被静默丢失，因此路线图中的动作级契约无法安全推进。

## What Changes

- 将 Atlas 的工具输入 schema 改为单一、完整的 JSON object 边界，允许保存任意 JSON Schema 2020-12 根级关键字。
- rmcp 适配层直接转交该 schema object，不再只投影 `type`、`properties` 与 `required`。
- 增加回归测试，证明合成的高级根级关键字和现有 15 项工具 schema 在 Atlas 合约与 rmcp wire model 之间保持完全一致。
- 保持现有 15 项工具的名称、顺序、描述、当前 schema wire shape 与 `tools/call` 行为不变。
- **BREAKING**：`atlas-mcp` 的公共 Rust 类型 `ToolInputSchema` 从固定字段结构改为封装完整 JSON object；直接使用结构体字段初始化或读取的下游代码需要迁移到构造器/对象访问接口。
- 更新路线图与 Unreleased changelog，记录无损 schema 边界完成。

## Capabilities

### New Capabilities

- `mcp-tool-schema-fidelity`: 定义 Atlas 工具输入 schema 在内部合约与 rmcp transport 之间的无损保存、转交和现有 wire 兼容要求。

### Modified Capabilities

- 无。

## Impact

- 代码：`crates/atlas-mcp/src/protocol.rs`、`crates/atlas-mcp/src/tools/tool_schemas.rs` 与 `crates/atlas-mcp/src/lib.rs`。
- 测试：现有 schema 访问方式迁移，并新增根级组合、条件、定义与附加属性关键字的 transport parity 回归。
- 文档：`docs/roadmap.md` 与 `CHANGELOG.md`。
- 依赖：不新增或升级依赖；继续复用 `serde_json::Map<String, Value>` 与 rmcp 的 `JsonObject`。
- 外部行为：当前工具目录 JSON 保持不变；该变更只消除未来高级 schema 关键字被静默截断的风险。