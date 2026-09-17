# mcp-mrtr-candidate-selection Specification

## Purpose
为真正需要用户选择的 `explore` 多候选结果提供标准 MCP 多轮输入流程，同时保持确定性请求、旧协议与非交互客户端的现有单轮行为和安全边界。

## Requirements

### Requirement: 候选选择必须按协议与客户端能力启用

当有效 MCP 协议版本是格式正确且不早于 `2026-07-28` 的 ISO `YYYY-MM-DD` 日期，且客户端声明支持表单 elicitation 时，系统 SHALL 能够为 `explore` 多候选结果返回 `input_required`。任一条件不满足时，系统 MUST 返回现有完整候选结果，不得发送 MRTR 中间结果。

#### Scenario: 非日期版本不得启用候选 MRTR

- **WHEN** 请求版本带非日期后缀、不是定宽 ISO 日期或包含无效月份
- **THEN** 系统不得仅凭字符串排序启用候选 MRTR

#### Scenario: 现代交互客户端遇到多个候选

- **WHEN** 客户端协商 `2026-07-28` 或更新协议、声明表单 elicitation，且 `explore` 的符号输入解析为多个候选
- **THEN** 系统返回 `resultType: "input_required"`，并包含一个候选选择输入请求

#### Scenario: 旧协议客户端保持兼容

- **WHEN** 客户端使用早于 `2026-07-28` 的协议调用会产生多个候选的 `explore`
- **THEN** 系统返回现有完整候选 JSON 结果，且不返回 `input_required`

#### Scenario: 现代非交互客户端保持兼容

- **WHEN** 客户端使用 `2026-07-28` 或更新协议但未声明表单 elicitation
- **THEN** 系统返回现有完整候选 JSON 结果，且不返回 `input_required`

#### Scenario: 确定性查询保持单轮完成

- **WHEN** `explore` 输入唯一解析到一个符号，无论客户端是否支持 MRTR
- **THEN** 系统返回正常完整工具结果，不发出候选选择输入请求

### Requirement: 输入请求必须完整表达有界候选

候选选择输入请求 MUST 包含现有 `explore` 歧义结果中每个有界候选的一项可选值，并 MUST 携带足以重建对应精确 `SymbolSelector` 的值。系统 SHALL 提供可读的候选标题，使用户能够区分限定名、文件和行号。

#### Scenario: 候选请求映射现有候选

- **WHEN** `explore` 歧义结果包含多个 `candidates[].symbol_ref`
- **THEN** 表单选择枚举按现有候选顺序包含对应数量的选择值，且每个值可恢复为相应的 JSON 对象选择器

#### Scenario: 工具目录与输入 schema 不变

- **WHEN** 客户端在该能力启用前后调用 `tools/list`
- **THEN** 15 项工具目录、工具顺序以及 `explore.inputSchema` 保持不变

### Requirement: 接受的选择必须完成原始查询

系统 MUST 将接受响应中的有效选择作为原 `explore` 请求的精确 `symbol` 输入，保留其他原始参数，并通过正常 `SymbolSelector` 解析与校验路径完成工具调用。由于系统不发出 `requestState`，客户端重试 MUST 重发原始参数对象；参数对象缺失时系统 MUST 返回 `invalid_params`，不得静默构造一个丢失原参数的新调用。

#### Scenario: 接受候选后完成 explore

- **WHEN** 客户端以 `accept` 响应候选请求，并提供可解析为 JSON 对象的选择值
- **THEN** 系统使用该对象替换原请求的 `symbol`、保留其余参数，并返回正常完整工具结果

#### Scenario: 畸形接受响应被拒绝

- **WHEN** `accept` 响应缺少选择字段、选择值不能解析为 JSON 对象，或重试没有重发原始参数对象
- **THEN** 系统返回 `invalid_params`，且不得猜测候选、静默选择候选或丢弃原请求参数

#### Scenario: 未知输入响应被拒绝

- **WHEN** MRTR 重试缺少系统发出的固定候选响应项，或携带无法识别的 action
- **THEN** 系统返回 `invalid_params`

### Requirement: 拒绝或取消不得形成 MRTR 循环

当用户拒绝或取消候选选择时，系统 SHALL 返回原有完整候选结果，并 MUST 抑制该重试再次返回 `input_required`。

#### Scenario: 用户拒绝选择

- **WHEN** 客户端以 `decline` 响应候选请求
- **THEN** 系统返回现有包含候选列表的完整 `explore` 结果

#### Scenario: 用户取消选择

- **WHEN** 客户端以 `cancel` 响应候选请求
- **THEN** 系统返回现有包含候选列表的完整 `explore` 结果

### Requirement: 无状态候选流程不得信任 requestState

该候选选择流程 SHALL 仅使用 `inputRequests` 与 `inputResponses`，MUST NOT 发出 `requestState`。系统 MUST 拒绝客户端为该流程提供的非空 `requestState`，不得解析或使用其内容影响工具执行。由于该流程无状态，系统 SHALL 将接受的选择视为普通、不可信的 `SymbolSelector` 输入，而不是与先前候选集合绑定的服务器断言；该值 MUST NOT 用于授权、项目切换或其他安全决策。

#### Scenario: 初始输入请求省略 requestState

- **WHEN** 系统返回 `explore` 候选选择的 `input_required`
- **THEN** 响应中不存在 `requestState`

#### Scenario: 无状态选择不作为服务器断言

- **WHEN** 客户端接受一个格式有效但不属于先前候选枚举的 `SymbolSelector`
- **THEN** 系统按普通 `explore` 输入的同一解析、校验和权限边界处理它，且不得将其作为授权或项目身份依据

#### Scenario: 客户端注入 requestState

- **WHEN** 客户端在 `explore` MRTR 重试中提供非空 `requestState`
- **THEN** 系统返回 `invalid_params`，且不执行基于该状态的候选选择

### Requirement: task 内候选输入必须复用现有候选语义

系统 SHALL 对直接 `InputRequiredResult` 与 task `input_required` 使用相同的候选 input id、候选顺序、序列化 `symbol_ref` 值、可读标题和响应校验。task 输入 MUST NOT 发出或信任 `requestState`，且接受值 MUST 继续作为普通、不可信的 `SymbolSelector` 由现有核心验证。

#### Scenario: task 候选请求与直接 MRTR 一致

- **WHEN** 同一有效多候选结果分别进入直接 MRTR 与 task 输入路径
- **THEN** 两者的唯一 input request 使用相同 id、表单字段、候选值顺序、标题和 requested schema

#### Scenario: 接受 task 候选

- **WHEN** 客户端通过 `tasks/update` 对候选 input id 提交 `accept`，并提供可解析为 JSON 对象的选择值
- **THEN** 系统以该对象替换原 `explore` 参数中的 `symbol`、保留其他参数，并在 task 固定项目上执行现有核心路径

#### Scenario: 拒绝或取消 task 候选

- **WHEN** 客户端通过 `tasks/update` 提交 `decline` 或 `cancel`
- **THEN** task 以触发输入的原完整候选结果进入 `completed`，不执行猜测选择或第二轮输入

#### Scenario: 畸形 task 输入响应

- **WHEN** task 候选响应缺少 action、使用未知 action、`accept` 缺少选择字段、选择值不能解析为 JSON 对象，或原始参数不是对象
- **THEN** task 以标准 `invalid_params` JSON-RPC error 进入 `failed`，且不执行候选后的核心调用

#### Scenario: 枚举外选择器仍是不可信输入

- **WHEN** 客户端接受一个格式有效但不属于先前候选枚举的 `SymbolSelector`
- **THEN** 系统按普通 `explore` 输入的同一解析、校验和项目边界处理它，不得将其作为授权或服务器断言

#### Scenario: 非 task 候选流程保持不变

- **WHEN** `explore` 直接返回多候选结果，或客户端未同时声明 Tasks 与表单能力
- **THEN** 现有直接 MRTR、完整候选结果和 `query_id` 兼容流程保持不变
