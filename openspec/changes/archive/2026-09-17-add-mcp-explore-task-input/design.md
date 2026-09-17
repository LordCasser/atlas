## Context

现有 Tasks 适配先执行同步工具调用；只有严格 retry ticket 且 query snapshot 可在当前项目固定时才创建 task。task 复用 `resume_query`，并把非 retry 的完整 `CallToolResult` 作为终态。`explore` 被排除，因为 Focus 重放可能返回 `ambiguous: true` 候选列表；直接把它标记为 completed 会失去现代表单客户端本可使用的候选选择 MRTR。

Atlas 已有无状态候选 MRTR：它从现有候选结果构造唯一 `elicitation/create` 表单，接受响应时把 JSON 对象选择器写回原参数，拒绝/取消时返回原候选，不发 `requestState`。rmcp `TaskContext::request_input()` 会让 task 进入 `input_required`，并等待对应 `tasks/update.inputResponses`；取消会清除 pending input 并使等待返回 `TaskExit::Cancelled`。

## Goals / Non-Goals

**Goals:**

- 只为同时支持 Tasks 与表单 elicitation 的现代客户端，把真实 `explore` Focus retry 提升为 task。
- 在同一个 task 内发布至多一次现有候选表单，并严格处理 `tasks/update` 响应。
- 接受候选后在原项目、原参数边界内完成查询；后续 Focus retry 仍留在同一 task。
- 保持无 Tasks、无表单、旧协议、快速歧义、直接核心调用和既有 MRTR 行为。

**Non-Goals:**

- 不建立通用 task 内 MRTR 框架，不把 project-path 表单或任意 elicitation 自动移入 task。
- 不支持一个 task 内多轮候选选择、采样或 roots 请求。
- 不取消共享 Focus 物化，不实现 task push、持久化或跨进程恢复。
- 不把客户端提交的候选值当作服务器断言、授权信息或项目身份。

## Decisions

### 1. `explore` task 使用 Tasks + form 双能力门控

普通 Focus 工具仍只要求现代协议和 Tasks Extension。`explore` 只有在同一请求还声明 `elicitation.form` 时才允许 taskify；否则保留原 `query_id` retry ticket。这样 task 后续遇到多候选时一定有协议能力发布 form input request，不会把进行中的选择需求误标成 completed。

快速返回的 `explore` 多候选结果仍走现有直接 `InputRequiredResult`，不会仅因为客户端支持 Tasks 而创建 task。

### 2. 初始调用、snapshot 与 task 固定到同一项目

对可能转换为 task 的项目作用域调用，适配层在进入同步核心前先读取一次当前 `ProjectSlot`，构造持有该 `Arc<ActiveProject>` 的固定 execution router。初始工具调用、Focus 状态、snapshot 写入和 task preparation 全部使用该 router；并发 `project(open)` 只能在线性化点之前或之后生效，不能把一个调用的结果与另一个项目的 snapshot 混合。

初始调用返回 retry ticket 后，适配层仍在同一个 blocking 闭包并持有共享并发 permit 时锁定固定项目的 snapshot map，一次性确认 query ID、克隆原 `tool_name` / `tool_args`，并构造 replay router。task 不在释放 permit 后重新读取可切换的外层 `ProjectSlot`。

对非 `explore` task，捕获的工具元数据只用于证明 snapshot 身份，不改变现有重放。对 `explore` task，原参数用于接受候选后的精确重试。

### 3. 候选请求构造与响应解析只保留一套规则

把现有候选结果到 `InputRequest::Elicitation` 的构造提取为共享 helper；直接 MRTR 将其包装成 `InputRequiredResult`，task 则直接传给 `TaskContext::request_input()`。固定 input id、候选顺序、序列化 `symbol_ref` 值和可读标题保持一致。

响应解析也提取为纯 helper：

- `accept` 必须包含可解析为 JSON 对象的选择器；该值替换原参数的 `symbol`，其他参数保持不变。
- `decline` / `cancel` 返回“放弃选择”分支。
- 缺失 action、未知 action、缺字段、非对象选择器或非对象原参数返回 `invalid_params`。

候选值继续按普通、不可信 `SymbolSelector` 交给核心解析，不因曾出现在表单中获得额外权限，也不要求 task 保存或信任 `requestState`。

### 4. task 状态机最多请求一次输入

`run_query_task` 在每次固定项目 `resume_query` 后按顺序处理：

1. 严格同 query ID retry marker：继续等待。
2. 第一次有效 `explore` 多候选结果：保存该完整结果，发布候选 input request，并在原 300 秒硬截止内等待 `tasks/update`。
3. `decline` / `cancel` 输入响应：以保存的完整候选 `CallToolResult` 进入 completed。
4. `accept`：在固定 router 上以修改后的原参数调用真实 `explore` 核心。终态直接 completed；若返回新的严格 retry ticket，必须能在同一固定项目找到 snapshot，然后同一 task 采用该内部 query ID 继续。
5. 已使用过候选输入后再次出现歧义：以完整候选结果完成，不发布第二个输入请求。

该状态机不创建嵌套 task，也不把内部旧/新 query ID 作为另一个进行中句柄暴露。不同 query ID 只在用户接受候选后、由同一固定项目上的新精确调用产生时允许；普通 `resume_query` 自行改变 query ID 仍是 task failure。

### 5. deadline、取消和错误语义覆盖输入等待

等待 input response、共享 permit 和阻塞 worker join 都必须与 task 硬截止竞争。到期返回 `TaskExit::Error`，TaskManager 清除 pending input 并把可见 task 标记为 failed。`tasks/cancel` 会清除 pending sender，使 `request_input()` 返回 Cancelled；task 不执行选择后的核心调用。

畸形 `tasks/update` 响应是适配器/协议错误，task 进入 failed；核心 `explore` 返回的 `isError: true` 仍是 completed task result。已经实际进入 `spawn_blocking` 的同步调用仍采用既有协作取消边界：Rust 无法强制停止它，但 task 不等待其返回才结算，后台调用继续持有 permit 直到自然退出。

## Risks / Trade-offs

- [接受候选后可能再次需要 Focus 物化] → 同一 task 只在固定项目验证新 snapshot 后采用内部 query ID，并继续受原 deadline 约束。
- [客户端永不回应 input request] → 独立硬截止结束 task，不依赖客户端再次轮询。
- [恶意客户端提交枚举外选择器] → 与现有无状态 MRTR 相同，按普通核心输入验证；不作为授权或项目身份。
- [多轮歧义造成输入循环] → 每个 task 只使用一个固定 input key 一次；后续歧义作为完整终态结果返回。
- [只有 Tasks、没有 form 的客户端不能获得 task] → 有意保留兼容 retry ticket，避免创建无法完成候选交互的 task。

## Migration Plan

1. 先提取候选请求/响应 helper 与固定 snapshot 元数据，保持现有直接 MRTR 测试全绿。
2. 扩展 `explore` task 门控和 task 状态机，覆盖 working → input_required → completed/failed/cancelled。
3. 运行完整 MCP/CLI 回归和独立审查；如需回滚，只恢复 `explore` 排除条件，现有非 task MRTR 与普通 task 不需迁移。
