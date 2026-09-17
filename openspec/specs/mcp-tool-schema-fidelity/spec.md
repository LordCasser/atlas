# mcp-tool-schema-fidelity Specification

## Purpose
为 Atlas MCP 工具输入契约提供无损的 JSON Schema object 边界，确保当前与未来的 JSON Schema 2020-12 根级关键字从工具注册到 rmcp 响应都不会被静默删除或改写。

## Requirements

### Requirement: Complete input schema objects are preserved
Atlas MUST 将每个工具注册的完整输入 schema object 原样传递到 MCP transport，不得只选择已知关键字，也不得删除未知或未来新增的根级关键字。

#### Scenario: Advanced root keywords cross the transport boundary
- **WHEN** 一个工具输入 schema 同时包含 `$schema`、`$defs`、`oneOf`、`if`、`then`、`else`、`additionalProperties` 与普通 `type`/`properties`/`required` 字段
- **THEN** rmcp 工具模型中的 `inputSchema` 与 Atlas 注册的 schema object 完全相等
- **AND** 每个根级关键字及其嵌套值保持不变

#### Scenario: Unknown future keyword is registered
- **WHEN** 工具输入 schema 包含 Atlas 与当前 rmcp 版本均未专门识别的根级关键字
- **THEN** 该关键字仍出现在 MCP 工具模型的 `inputSchema` 中
- **AND** transport 适配层不依据关键字白名单过滤它

### Requirement: Input schema remains an object contract
Atlas 的工具输入 schema MUST 以 JSON object 表示，并 MUST 能由调用方作为完整对象读取或转交；不得把 schema 状态拆分到会造成信息丢失的并行字段中。

#### Scenario: Construct an object-shaped schema
- **WHEN** 调用方使用完整 JSON object 构造工具输入 schema
- **THEN** Atlas 保留该对象的全部键值
- **AND** 序列化后的 `inputSchema` 仍为 JSON object

### Requirement: Existing catalog wire shape remains compatible

迁移到无损 schema 边界后，Atlas MUST 保持当前 15 项工具的名称、顺序、描述、属性集合和顶层 unconditional `required` 不变，并 MUST 保持 `tools/call` 行为不变。工具定义有意新增的 JSON Schema 2020-12 动作级条件 MUST 作为完整 schema 内容原样到达 transport。

#### Scenario: Compare registered and transported current schemas
- **WHEN** 当前 Atlas 二进制构造完整工具目录并转换为 rmcp 工具模型
- **THEN** 每个工具的 transport `inputSchema` 与其 Atlas 注册 schema 完全相等
- **AND** 工具目录仍包含相同顺序的 15 项工具

#### Scenario: Existing schema consumers inspect properties and required fields
- **WHEN** 测试或下游代码通过完整 schema object 读取 `properties` 与顶层 `required`
- **THEN** 获得的参数集合和 unconditional 必填字段集合与变更前一致
- **AND** `domain_rules` 与 `fp_dispatches` 的动作级必填参数由新增条件关键字表达
