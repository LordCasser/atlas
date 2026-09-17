## Context

`ToolInputSchema` 当前以三个 Rust 字段表示根级 `type`、`properties` 与 `required`，`AtlasMcpService::to_rmcp_tool` 再手工创建新的 JSON object。工具属性内部已有 `oneOf` 等组合约束，但根级只能表达这三个字段；任何其他关键字即使在上游构造，也没有可跨越该边界的存储位置。

rmcp 3.0.1 的 `Tool::new_with_raw` 接受完整 `JsonObject`，因此限制来自 Atlas 自身的数据模型，而不是 SDK。MCP 工具的 `inputSchema` 本身要求 object-shaped JSON Schema，现有 15 项工具也全部以 `type: "object"` 为根。

## Goals / Non-Goals

**Goals:**

- 用一个对象拥有全部 schema 根级键值，消除字段投影与双重表示。
- 保留现有 schema 构造的可读性，并让高级根级关键字可直接表达。
- 让 Atlas 合约序列化和 rmcp model 共享同一份 schema 内容。
- 通过合成高级 schema 与完整目录 parity 测试锁定无损边界。

**Non-Goals:**

- 本次不为现有工具新增动作级 `if`/`then`/`else` 约束；只建立不会丢失这些约束的基础边界。
- 不在 Atlas 内实现 JSON Schema 校验器，也不判断关键字组合在语义上是否有效。
- 不增加 `outputSchema`，不改变工具处理器的参数解析或错误语义。
- 不保证 JSON object 的文本键顺序；契约是键值结构完全相等。

## Decisions

### 1. `ToolInputSchema` 使用透明的完整对象封装

将固定字段结构替换为封装 `serde_json::Map<String, Value>` 的透明 newtype。类型提供：

- 接收完整 map 的构造入口；
- 为当前工具保留 `object(properties, required)` 便利构造器；
- 只读对象访问与消耗式 `into_object()` 转交接口。

这样 object-shaped 不变量由类型边界表达，同时允许 `$schema`、`$defs`、组合、条件、数值约束及未知未来关键字共存。

**Alternative: 直接使用 `serde_json::Value`。** 该方案允许数组、标量或布尔根值进入 MCP `inputSchema`，随后仍需运行时检查并转换为 rmcp 的 `JsonObject`，因此不采用。

**Alternative: 保留三个字段并增加 `#[serde(flatten)] extra`。** 该方案仍有两个并行状态来源，并继续把 `type` 限制为字符串；字段冲突与覆盖规则也会增加不必要复杂度，因此不采用。

### 2. transport 直接移动对象，不做 serde round-trip 或关键字投影

`to_rmcp_tool` 消耗 Atlas `Tool` 后，把 `ToolInputSchema::into_object()` 的结果直接传给 `rmcp::model::Tool::new_with_raw`。适配层不再理解任何 JSON Schema 关键字。

这把职责划分为：工具注册代码拥有 schema 内容，`ToolInputSchema` 保证 object-shaped 表示，transport 只负责协议模型转换。未来新增关键字不需要同步修改适配器。

**Alternative: 先把 `ToolInputSchema` 序列化成 `Value` 再取 object。** 虽然也能保留字段，但引入一次无意义的序列化/反序列化路径和可失败分支，不如直接移动 map。

### 3. 当前工具继续通过 object 便利构造器生成相同 wire shape

15 项现有定义只把结构体字面量迁移为 `ToolInputSchema::object(properties, required)`。该构造器按当前语义生成 `type: "object"`、`properties`，并仅在必填列表存在时生成 `required`，因此序列化结果不新增 `null`、空数组或元数据字段。

所有读取固定字段的测试改为从完整对象读取对应关键字；测试意图保持不变。

### 4. 测试同时覆盖合成能力与真实目录

- 合成 schema 包含 `$schema`、`$defs`、根级 `oneOf`、`if`/`then`/`else`、`additionalProperties` 和一个未知扩展关键字，断言 rmcp `input_schema` 与原 map 完全相等。
- 遍历真实 15 项目录，逐项断言 Atlas schema object 与 rmcp model 完全相等，并锁定名称和顺序没有改变。
- 现有 schema 参数/必填字段/嵌套 `oneOf` 测试继续运行，证明迁移未改变现有契约。

## Risks / Trade-offs

- [公共 Rust API 发生源码不兼容] → 在 proposal 与 changelog 明确标记；仓库内所有调用点同批迁移，编译器负责发现遗漏。
- [完整对象允许注册语义无效的 JSON Schema] → 本次能力只承诺 fidelity，不承诺 validity；继续由定义代码和现有 schema 测试负责正确性，未来如需验证器单独立项。
- [未来代码绕过便利构造器造成现有 wire drift] → 真实目录 parity 与既有 shape 测试锁定当前外部契约。
- [map 键顺序可能变化] → 使用结构相等而非 JSON 文本相等；MCP 与 JSON Schema 均不赋予 object 键顺序语义。

## Migration Plan

无需数据迁移。一次性修改公共类型、15 项工具构造、transport 转换与受影响测试；验证通过后发布为下一版本的源码级 breaking change。回滚时恢复固定字段结构和投影逻辑即可，不涉及持久化数据或运行时状态。