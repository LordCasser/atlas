## Context

Atlas 的 `domain_rules` 与 `fp_dispatches` 都在一个工具名下复用多个 action。handler 当前拥有明确的运行时不变量：

- `domain_rules(add)` 拒绝缺少 `rule_kind` 或 `pattern` 的调用；
- `domain_rules(delete)` 拒绝缺少 `rule_id` 的调用；
- `fp_dispatches(add)` 拒绝缺少 `field_qname` 或 `target_qname` 的调用；
- `fp_dispatches(delete)` 接受 `annotation_id`、`field_qname` 或两者同时存在，并优先使用 `annotation_id`；
- 两个工具都把缺少 `action` 解释为只读 `list`。

当前 schema 只把这些规则写进 description。此前 Atlas 的固定字段 schema 表示无法承载根级条件；`mcp-tool-schema-fidelity` 已将输入 schema 改为完整 JSON object，且 rmcp 3.0.1 的 raw schema API会原样接收该 object，因此限制现在只剩工具定义内容。

JSON Schema 2020-12 的 `properties` 不会要求属性存在。若 `if` 只写 `properties.action.const`，缺少 `action` 时条件仍可能成立并错误触发 `then`。每个动作条件因此必须同时包含 `required: ["action"]`。

## Goals / Non-Goals

**Goals:**

- 让支持 JSON Schema 条件关键字的 MCP 客户端在发送请求前发现动作级缺参。
- 使 schema 与既有 handler 接受/拒绝边界一致。
- 保持缺少 `action` 的默认 list、delete 双标识兼容和运行时防御。
- 锁定 Atlas 注册对象与 rmcp transport 中的完整条件结构。

**Non-Goals:**

- 不在 Atlas 服务端增加通用 JSON Schema validator；handler 仍负责权威运行时校验。
- 不使用 `additionalProperties: false`，不禁止与当前 action 无关但既有 handler 会忽略的字段。
- 不把 `action` 改为顶层 unconditional required。
- 不改变 mutation 确认、工具分派、错误文案、数据库写入或 graph refresh。
- 不为 `project(open)` 等其他复合工具顺手增加条件约束。

## Decisions

### 1. 使用根级 `allOf` 承载独立动作条件

每个动作使用一个 `if`/`then` 分支：

```json
{
  "if": {
    "properties": {"action": {"const": "add"}},
    "required": ["action"]
  },
  "then": {"required": ["..."]}
}
```

根级 `allOf` 让 add/delete 条件彼此独立，也保留现有 `type`、`properties` 与 optional top-level `required` 结构。工具属性集合和 action enum 不变。

**Alternative: 根级 `oneOf` 为每个 action 重复完整 schema。** 该方案会复制属性定义、description 和约束，增加漂移风险，因此不采用。

### 2. `fp_dispatches(delete)` 使用 `anyOf`，不使用 `oneOf`

运行时在 `annotation_id` 非空时优先按 ID 删除，否则按 `field_qname` 查找；两者同时提供是合法输入。`then.anyOf` 的两个分支分别要求一个字段，准确表达“至少一个”。`oneOf` 会拒绝两者同时存在，改变既有兼容语义。

### 3. 条件只前移必然失败，不取代运行时校验

Schema 只表达字段存在性。空字符串、符号不存在、类型/语言不适用、ID 不存在等仍由 handler 判定。即使客户端不支持或不执行条件 schema，服务端行为也与变更前一致。

### 4. 回归锁定语义结构与 transport 保真

测试将：

- 精确断言两个工具的根级 `allOf`；
- 验证每个 `if` 都要求 `action` 存在，避免默认 list 被误约束；
- 验证 delete 的 `anyOf` 是至少一个标识而非互斥；
- 保留现有属性集合、顶层 required、15 项名称/顺序测试；
- 通过既有 Atlas→rmcp 完整 schema parity 测试证明条件关键字未丢失，并增加代表性 wire 断言。

不新增 JSON Schema validator 依赖；本切片验证生成的标准结构及无损 transport，运行时 handler 测试继续验证权威执行边界。

## Risks / Trade-offs

- [部分客户端不实现 `if`/`then`] → handler 运行时校验保留；这些客户端只是无法提前发现错误，不会获得不同服务端语义。
- [契约收紧暴露旧客户端缺参调用] → 只拒绝当前 handler 已必然拒绝的输入，并在 changelog 明确说明。
- [缺少 action 时误触发 add/delete 条件] → 每个 `if` 同时要求 `action`，并用回归锁定。
- [两种 delete 标识被错误建模为互斥] → 使用 `anyOf`，允许同时提供并保持 annotation ID 优先语义。
- [条件在 transport 中被投影] → 完整对象 parity 与 wire 回归锁定根级 `allOf` 及嵌套关键字。

## Migration Plan

无需数据迁移。更新两个工具注册 schema 与回归测试，串行完成 MCP/CLI/Clippy 验收后归档。回滚只需删除根级条件，handler 与持久化状态无需变化。
