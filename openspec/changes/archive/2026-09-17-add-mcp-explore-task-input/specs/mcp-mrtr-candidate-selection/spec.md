## ADDED Requirements

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
