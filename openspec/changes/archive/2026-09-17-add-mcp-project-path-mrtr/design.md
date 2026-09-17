## Context

`project` 是一个多 action 工具；`project_path` 只对 `action="open"` 必需，因此当前 V1 `inputSchema` 没有把它放入全局 `required`。核心处理器在路径缺失时返回 `Missing required parameter: project_path`，路径存在时负责 canonicalize、长度/目录检查、创建 `.atlas`、打开 SQLite 并初始化 schema。

Atlas 已有一个版本与表单能力门控的无状态 `explore` MRTR。该变化应复用同一协议边界，但路径缺失可以在调用核心前由参数确定，无需先制造一次预期错误或修改 `ToolRouter`。

## Goals / Non-Goals

**Goals:**

- 仅在现代协议、显式表单能力和 `project(action="open")` 真正缺失路径时请求一个字符串路径。
- 接受路径后复用现有项目打开实现及其全部校验、副作用和错误语义。
- 保持旧协议、非交互客户端、其他 action、已有路径与畸形路径类型的现有行为。
- 不保存或信任客户端回显的服务器状态。

**Non-Goals:**

- 不自动为任意项目作用域工具打开项目，也不猜测当前工作目录。
- 不改变 `project.inputSchema`、项目切换并发语义、数据库位置或索引策略。
- 不为 include roots、search scope、mutation confirmation 或其他参数建立通用表单框架。

## Decisions

### 1. 在 rmcp 适配层做调用前的窄 preflight

当工具名为 `project`、`action` 恰为 `open`、`project_path` 缺失或是空字符串，并且本轮没有输入响应时，适配层在执行 `ToolRouter::call_tool()` 前判断是否返回路径 `InputRequiredResult`。只有现有 SEP-2322 日期门控与 `elicitation.form` 能力同时满足时才启用；否则继续执行核心并返回原有完整错误结果。

错误类型的 `project_path`（例如数字、数组）不是“缺失输入”，MUST 继续交给核心处理器，不发 MRTR。这样不会把参数类型错误伪装成可交互补全。

### 2. 表单只包含一个受限字符串字段

`inputRequests` 使用固定输入标识，表单包含一个 required `project_path` 字符串，`minLength: 1`、`maxLength: 4096`，并明确提示应提供绝对项目目录。该 schema 改善支持校验的客户端 UI，但服务器仍以现有 `handle_open_project()` 为最终权威，不依赖客户端 schema validation。

### 3. 接受响应只补充路径并调用现有核心

客户端重试必须保留原参数对象：

- `accept` 必须携带非空字符串路径；适配层将其写入原参数的 `project_path`，保留 `action` 和其他字段，然后执行一次正常 `project(open)`。
- `decline` 或 `cancel` 不改写参数，并禁止该重试再次发出路径 MRTR，因此核心返回现有缺失路径错误，不形成循环。
- 固定响应缺失、数量错误、未知 action、非字符串/空路径或缺失原参数对象返回 `invalid_params`。

路径是不可信用户输入，最终必须经过现有 canonicalize、最大长度、目录可访问性、`.atlas` 创建、SQLite 和 schema 检查。MRTR 不扩大直接调用 `project(open)` 已有的文件系统权限。

### 4. 不发出 requestState

路径值本身就是完成调用所需的全部新输入，官方 rmcp 高级客户端会克隆原始 `CallToolRequestParams` 并只填充 `inputResponses`。响应省略 `requestState`；该流程收到的非空 state 返回 `invalid_params`，不解析、不持久化，也不用于项目身份。

### 5. 与候选选择保持流程隔离

项目路径使用独立 input id 与字段名。`explore` 候选响应、project status/files、已有非空路径的 open 以及其他工具不进入本流程。适配层可以共享协议/表单能力 helper，但不得让一个流程的响应被另一个流程消费。

## Risks / Trade-offs

- [接受路径会创建 `.atlas` 并打开数据库] → 仅在用户显式调用 `project(open)` 且接受表单后发生，完全复用现有副作用边界；decline/cancel 不打开项目。
- [相对路径或不可访问路径通过表单] → 表单文案提示绝对路径，服务器仍由现有核心返回权威错误，不复制路径规则。
- [客户端不支持表单] → 保持现有缺失路径完整错误，不降低兼容性。
- [无状态重试丢失原 action] → 要求重发原参数对象；缺失对象返回 `invalid_params`，不得猜测 `action=open`。
- [适配层 MRTR 分支继续增加] → 本切片只增加一个窄 preflight，不引入通用动态表单抽象；达到第三个重复用例前不抽象。

## Migration Plan

1. 加入路径 preflight helper 和纯单元/wire 测试，锁定触发条件、表单形状和重试分支。
2. 接入 `ServerHandler::call_tool()` 的核心调用前路径，验证接受后由真实项目处理器打开临时目录，decline/cancel 与兼容路径不变。
3. 如需回滚，只移除适配层 project-path 分支；工具 schema、核心路由与持久化格式无需迁移。