## Why

`explore` 在符号名称对应多个定义时只能返回候选列表，客户端必须自行拼接第二次工具调用；MCP 2026-07-28 的 MRTR 可以把这类“必须由用户选择后才能继续”的交互表达为标准 `input_required`，同时不能改变旧协议与非交互客户端的既有行为。

## What Changes

- 对已协商 MCP `2026-07-28` 或更高版本、且声明支持表单 elicitation 的客户端，在 `explore` 遇到多候选符号时返回标准 `input_required` 候选选择请求。
- 接受候选选择响应后，将所选 `SymbolSelector` 作为本次 `explore` 的精确符号输入，并保留原请求的其余参数完成查询。
- 对确定性 `explore` 请求、旧协议客户端、未声明表单 elicitation 的客户端，以及用户拒绝或取消选择的流程，保持现有单轮完整结果语义。
- 该候选流程不发出 `requestState`；客户端提供的选择仍按普通 `SymbolSelector` 输入校验，避免引入需要签名、过期和重放防护的服务器状态。
- 不改变 15 项工具目录、`explore` 的 `inputSchema`、Atlas 核心路由结果格式或其他工具行为。

## Capabilities

### New Capabilities

- `mcp-mrtr-candidate-selection`: 定义版本与客户端能力感知的 `explore` 候选选择 MRTR、重试及兼容回退语义。

### Modified Capabilities

<!-- 无。 -->

## Impact

- 影响 `crates/atlas-mcp/src/lib.rs` 的 rmcp 传输适配层及其单元/协议回归测试。
- 复用 rmcp 3.0.1 已提供的 MRTR 与 elicitation 模型，不升级依赖，不新增持久化状态。
- `ToolRouter::call_tool()`、现有 JSON 响应封装、工具 schema 与旧协议 wire shape 保持不变。