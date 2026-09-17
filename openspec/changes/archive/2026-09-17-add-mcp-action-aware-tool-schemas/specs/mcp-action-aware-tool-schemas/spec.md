## ADDED Requirements

### Requirement: 复合工具必须公开动作级必填参数

Atlas MUST 在 `domain_rules` 与 `fp_dispatches` 的 MCP `inputSchema` 中使用 JSON Schema 2020-12 条件关键字表达与现有 handler 一致的动作级字段存在性约束。系统 MUST 保留 handler 运行时校验，不得把客户端 schema 校验视为唯一防线。

#### Scenario: domain rule add 声明所需字段

- **WHEN** 客户端检查 `domain_rules` 且 `action` 为 `add`
- **THEN** schema 要求同时提供 `rule_kind` 与 `pattern`

#### Scenario: domain rule delete 声明规则 ID

- **WHEN** 客户端检查 `domain_rules` 且 `action` 为 `delete`
- **THEN** schema 要求提供 `rule_id`

#### Scenario: function-pointer dispatch add 声明两端符号

- **WHEN** 客户端检查 `fp_dispatches` 且 `action` 为 `add`
- **THEN** schema 要求同时提供 `field_qname` 与 `target_qname`

#### Scenario: function-pointer dispatch delete 接受至少一个标识

- **WHEN** 客户端检查 `fp_dispatches` 且 `action` 为 `delete`
- **THEN** schema 要求 `annotation_id` 或 `field_qname` 至少一个
- **AND** 两者同时存在仍是合法结构

### Requirement: 默认读操作不得被条件约束破坏

Atlas MUST 保持缺少 `action` 时两个工具默认执行 `list` 的现有契约。每个条件 `if` SHALL 显式要求 `action` 属性存在，不得仅依靠 `properties.action.const` 判断动作。

#### Scenario: 省略 action 继续列出 domain rules

- **WHEN** 调用参数省略 `domain_rules.action`
- **THEN** schema 不要求 add/delete 专属字段，handler 继续执行 list

#### Scenario: 省略 action 继续列出 FP annotations

- **WHEN** 调用参数省略 `fp_dispatches.action`
- **THEN** schema 不要求 add/delete 专属字段，handler 继续执行 list

#### Scenario: 显式 list 与 learn 保持原参数语义

- **WHEN** `domain_rules.action` 为 `list` 或 `learn`，或 `fp_dispatches.action` 为 `list`
- **THEN** schema 不增加 mutation 专属必填字段

### Requirement: 条件 schema 必须无损到达 MCP transport

Atlas MUST 将两个工具的根级 `allOf` 以及嵌套 `if`、`then`、`required`、`anyOf` 原样传递到 rmcp tool model 和 `tools/list` wire。工具数量、顺序、名称、属性集合与顶层 unconditional `required` SHALL 保持不变。

#### Scenario: 注册 schema 转换为 rmcp tool

- **WHEN** Atlas 把当前工具目录转换为 rmcp tool models
- **THEN** `domain_rules` 与 `fp_dispatches` 的完整 `inputSchema` 与注册对象结构相等
- **AND** 动作级条件关键字及嵌套值均未被删除或改写

#### Scenario: 比较变更前后的目录兼容边界

- **WHEN** 比较工具目录的名称、顺序、属性键和顶层 required
- **THEN** 仍为同一 15 项目录与同一参数集合
- **AND** 唯一有意新增的 schema 内容是两个工具的动作级条件

### Requirement: 服务端执行语义必须保持不变

Atlas MUST 保持两个 handler 对合法输入、空字符串、未知 action、符号解析、删除标识优先级、持久化写入与错误返回的现有行为。Schema SHALL 只把字段存在性错误前移给支持条件校验的客户端。

#### Scenario: 客户端不执行条件 schema

- **WHEN** 客户端发送缺少动作必填字段的调用
- **THEN** 现有 handler 仍拒绝该调用，不得因新增 schema 删除运行时检查

#### Scenario: delete 同时提供两个标识

- **WHEN** `fp_dispatches(delete)` 同时提供 `annotation_id` 与 `field_qname`
- **THEN** schema 接受该结构，handler 继续按现有规则优先使用 `annotation_id`
