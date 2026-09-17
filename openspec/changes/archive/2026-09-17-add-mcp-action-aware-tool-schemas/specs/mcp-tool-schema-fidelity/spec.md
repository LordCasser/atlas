## MODIFIED Requirements

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
