# mcp-overlay-delete-confirmation Specification

## Purpose
为现代交互客户端的持久化 overlay 删除提供一次明确、绑定原项目与原参数、短时且不可重放的 MCP 确认，同时保持核心删除和兼容路径不变。

## Requirements

### Requirement: 删除确认必须按工具形状、协议、能力和项目状态启用

系统 SHALL 仅在有效协议版本不早于 `2026-07-28`、客户端声明表单 elicitation、活动项目存在，且初始调用是合法形状的 `domain_rules(action="delete")` 或 `fp_dispatches(action="delete")` 时返回 `input_required`。任一条件不满足时，系统 MUST 保持现有直接核心结果语义。

#### Scenario: 现代客户端删除 domain rule

- **WHEN** 现代 form-capable 客户端在活动项目调用 `domain_rules` 的 `delete` action，并提供非空字符串 `rule_id`
- **THEN** 系统返回删除确认，不先调用核心删除

#### Scenario: 现代客户端删除 FP dispatch

- **WHEN** 现代 form-capable 客户端在活动项目调用 `fp_dispatches` 的 `delete` action，并提供非空字符串 `annotation_id`，或在其缺失时提供非空字符串 `field_qname`
- **THEN** 系统返回删除确认，不先删除 annotation、重建边或刷新图

#### Scenario: 非 delete 与畸形目标不触发确认

- **WHEN** action 不是 `delete`，或对应删除目标缺失、为空或类型错误
- **THEN** 系统按现有核心语义处理，不把参数错误伪装成确认流程

#### Scenario: 兼容客户端保持直接调用

- **WHEN** 协议早于 `2026-07-28`、客户端未声明 form elicitation，或当前没有活动项目
- **THEN** 系统按现有路径直接调用核心，并保持原成功或错误结果

### Requirement: 确认表单必须明确项目、目标和持久化影响

确认响应 MUST 包含唯一固定 input id `confirm_overlay_delete` 和 required boolean 字段 `confirm`，其默认值 MUST 为 `false`。message SHALL 展示固定项目、删除类别和精确目标，并明确操作会修改项目的持久化 Atlas 数据。

#### Scenario: domain rule 确认 wire shape

- **WHEN** 系统为 `domain_rules(delete)` 构造确认
- **THEN** form schema 只要求 boolean `confirm`，message 包含 canonical 项目路径和原 `rule_id`
- **AND** `input_required` 包含非空 opaque `requestState`

#### Scenario: FP 两种目标可区分

- **WHEN** FP delete 使用 `annotation_id` 或 `field_qname`
- **THEN** message 按现有核心优先级显示真正将被删除的目标种类和值，不得把两种模式混淆

### Requirement: requestState 必须有界、短时、不可预测且一次性

系统 MUST 把 `requestState` 仅作为不可预测的服务端 opaque handle；可信工具名、完整参数和固定项目 MUST 留在服务端。pending confirmation MUST 在 300 秒后过期、总量不得超过 128，并 MUST 在第一次兑换时原子移除。

#### Scenario: 正常一次性兑换

- **WHEN** 客户端在 TTL 内第一次提交已签发 handle
- **THEN** 系统原子取得并移除对应状态，再处理响应；同一 handle 的任何后续提交返回 `invalid_params`

#### Scenario: 过期、未知和服务重启状态

- **WHEN** handle 已过期、从未签发，或签发它的服务实例已经退出
- **THEN** 系统返回 `invalid_params`，且不得执行任何核心删除

#### Scenario: 并发重放

- **WHEN** 两个请求并发兑换同一 handle
- **THEN** 至多一个请求可进入响应处理和核心尝试，其余请求返回 `invalid_params`

#### Scenario: pending 数量达到上限

- **WHEN** 128 个未过期 confirmation 尚未兑换
- **THEN** 新的初始 delete 返回有界资源错误，不签发第 129 个状态，也不执行删除

### Requirement: 接受确认必须绑定完整参数和固定项目

`accept` 且 `confirm: true` 的重试 MUST 精确匹配状态中保存的工具名与完整原参数，并 MUST 在签发状态时固定的 `ActiveProject` 上执行一次现有核心删除。客户端回显参数、表单内容或 state 不得改变工具、目标、项目或其他参数。

#### Scenario: 接受后复用 domain rule 核心

- **WHEN** 客户端按原工具和完整参数接受 domain rule 删除
- **THEN** 系统在固定项目调用一次现有 `handle_domain_rules()` 删除路径，并保持其成功、not-found 和数据库错误语义

#### Scenario: 接受后复用 FP 核心

- **WHEN** 客户端按原工具和完整参数接受 FP dispatch 删除
- **THEN** 系统在固定项目调用一次现有删除、边清理和图刷新路径，并保持其现有结果语义

#### Scenario: 两轮之间切换项目

- **WHEN** 签发确认后外层服务打开另一个项目，再提交有效接受响应
- **THEN** 删除只作用于签发确认的原项目，不读取或修改新项目 overlay

#### Scenario: 参数或工具被改写

- **WHEN** 重试改变 action、目标、任意其他参数或工具名
- **THEN** 系统返回 `invalid_params`，已消费该 handle，且两个项目都不执行删除

### Requirement: 拒绝、取消和显式 false 必须安全完成

`decline`、`cancel` 或 `accept` 且 `confirm: false` SHALL 返回完整、非错误的 `status: "not_deleted"` 结果，并 MUST NOT 调用核心删除。该结果 MUST NOT 再次发出确认。

#### Scenario: 用户拒绝或取消

- **WHEN** 用户 decline、cancel 或接受表单但未把 `confirm` 设为 true
- **THEN** 系统返回未删除结果，原 rule/annotation、物化边和图保持不变

#### Scenario: 畸形响应被拒绝

- **WHEN** response 缺失、数量不是一、input id 错误、action 未知、accept 缺少 boolean `confirm`，或 requestState 缺失
- **THEN** 系统返回 `invalid_params`，消费已找到的 handle，且不执行删除

### Requirement: 确认必须与授权、工具目录和其他控制面隔离

系统 MUST 将 confirmation 视为误操作防护而非授权证明。确认能力 MUST NOT 改变工具 schema、核心 `ToolRouter::call_tool()`、Tasks、其他 MRTR 或 overlay 数据模型；非参与流程收到 input response/state 时 MUST 返回 `invalid_params` 而不是静默执行核心。

#### Scenario: 直接核心契约不变

- **WHEN** 直接调用 `ToolRouter::call_tool()`，或比较变更前后的 `tools/list`
- **THEN** 15 项目录/schema 与两个核心 delete 的原直接结果保持不变

#### Scenario: confirmation 不是 authorization

- **WHEN** 旧协议或无 form 客户端直接调用 delete
- **THEN** 系统仍按兼容路径执行核心；文档不得宣称 confirmation 提供身份、权限或恶意客户端防护

#### Scenario: 流程响应隔离

- **WHEN** confirmation state/response 被提交给 `project`、`explore` 或其他工具，或其他流程响应被提交给 delete confirmation
- **THEN** 系统返回 `invalid_params`，不得执行错误工具、消费另一个流程的输入或创建 task
