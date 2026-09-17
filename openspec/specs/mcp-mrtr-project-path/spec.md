# mcp-mrtr-project-path Specification

## Purpose
为缺少 `project_path` 的显式 `project(action="open")` 调用提供标准 MCP 多轮字符串输入，同时复用现有项目打开安全校验并保持旧协议与其他调用行为不变。

## Requirements

### Requirement: 路径补全必须按调用形状、协议与能力启用

系统 SHALL 仅在工具为 `project`、`action` 为 `open`、`project_path` 缺失或为空字符串、有效协议版本不早于 `2026-07-28` 且客户端声明表单 elicitation 时返回 `input_required`。任一条件不满足时，系统 MUST 保持现有完整工具结果语义。

#### Scenario: 现代交互客户端缺少项目路径

- **WHEN** 现代客户端声明表单 elicitation，并调用 `project` 的 `open` action 但未提供非空 `project_path`
- **THEN** 系统返回 `resultType: "input_required"` 和一个项目路径表单，不先执行项目打开

#### Scenario: 旧协议或非交互客户端缺少路径

- **WHEN** 旧协议客户端或未声明表单 elicitation 的客户端调用缺少路径的 `project(open)`
- **THEN** 系统返回现有 `Missing required parameter: project_path` 完整错误结果，不返回 MRTR

#### Scenario: 非 open action 不触发路径表单

- **WHEN** 客户端调用 `project(status)`、`project(files)` 或其他 action
- **THEN** 系统按现有核心语义处理，不请求 `project_path`

#### Scenario: 错误类型路径不伪装成缺失输入

- **WHEN** `project_path` 存在但不是字符串
- **THEN** 系统按现有核心错误语义处理，不返回路径 MRTR

### Requirement: 路径表单必须表达服务器输入边界

路径输入请求 MUST 包含一个 required 字符串字段 `project_path`，最小长度为 1，最大长度与 Atlas 当前项目路径上限 4096 一致，并 SHALL 提示用户提供绝对项目目录。响应 MUST NOT 包含 `requestState`。

#### Scenario: 路径表单 wire shape

- **WHEN** 系统为缺失路径返回 `input_required`
- **THEN** 唯一输入请求是 form elicitation，`project_path` schema 包含 `type: "string"`、`minLength: 1`、`maxLength: 4096` 和 required 声明
- **AND** 响应中不存在 `requestState`

### Requirement: 接受路径必须复用现有项目打开处理器

系统 MUST 将接受响应中的非空路径写入原请求参数，保留其他参数，并通过现有 `project(open)` 核心路径执行 canonicalize、长度、目录、数据库和 schema 校验。

#### Scenario: 接受有效目录后打开项目

- **WHEN** 客户端接受路径请求、重发原参数对象并提供一个可访问目录
- **THEN** 系统使用该目录完成正常 `project(open)`，返回现有成功结果，并激活同一 canonical 项目路径

#### Scenario: 接受的路径仍由核心校验

- **WHEN** 客户端接受一个相对、过长、不存在、不可访问或非目录路径
- **THEN** 系统返回现有项目打开错误，不得绕过或复制核心路径校验

#### Scenario: 畸形路径响应被拒绝

- **WHEN** `accept` 缺少路径、路径不是字符串、路径为空，或重试没有重发原参数对象
- **THEN** 系统返回 `invalid_params`，且不打开或切换项目

### Requirement: 拒绝、取消和流程隔离必须保持兼容

拒绝或取消路径输入后，系统 SHALL 执行一次原始缺失路径调用并返回现有完整错误，MUST NOT 再次返回 `input_required`。路径流程 MUST 只消费自己的固定输入响应，不得影响 `explore` 候选流程或其他工具。

#### Scenario: 用户拒绝或取消路径输入

- **WHEN** 客户端以 `decline` 或 `cancel` 响应项目路径请求
- **THEN** 系统返回现有缺失路径错误，且不打开项目、不形成 MRTR 循环

#### Scenario: 错误流程响应被拒绝

- **WHEN** project-path 重试缺少固定输入响应、携带额外响应、未知 action 或非空 `requestState`
- **THEN** 系统返回 `invalid_params`，且不得把该响应交给候选选择或核心项目打开逻辑

#### Scenario: 目录与核心契约不变

- **WHEN** 比较该能力启用前后的 `tools/list` 与 `ToolRouter::call_tool()` 直接结果
- **THEN** 15 项工具目录、`project.inputSchema` 和核心直接调用结果保持不变
