## Context

见 `proposal.md` 的 Why。当前 `ToolRouter::call_tool()` 对 `explore` 歧义返回成功 JSON，其中包含有界的 `candidates[].symbol_ref`；rmcp 适配层始终把该结果转换成完整的 `CallToolResult`。rmcp 3.0.1 已支持 `CallToolResponse::InputRequired`、表单 elicitation、`inputResponses` 和协议版本/客户端能力读取，但 Atlas 尚未使用这些能力。

该变化跨越 Atlas 同步工具结果与 rmcp 异步协议适配边界，并涉及不可信的客户端重试输入，因此需要在适配层明确能力门控和信任边界。

## Goals / Non-Goals

**Goals:**

- 仅在现代协议且客户端支持表单 elicitation 时，把 `explore` 的真实多候选结果提升为 MRTR 候选选择。
- 复用现有候选 `symbol_ref` 和正常 `explore` 校验路径完成重试，不复制符号解析算法。
- 保持旧协议、非交互客户端、确定性查询及 Atlas 核心路由的现有结果。
- 不持有或信任客户端回显的服务器状态。

**Non-Goals:**

- 不为 `search`、`symbol(context)`、缺失项目上下文或确认流程增加 MRTR。
- 不改变 `explore` 的工具 schema、候选排序、候选上限或 JSON 完整结果格式。
- 不引入跨进程恢复、一次性 nonce、持久化 MRTR 会话或通用交互框架。

## Decisions

### 1. MRTR 仅由 rmcp 适配层启用

适配层先按现有路径执行 `ToolRouter::call_tool()`。当且仅当工具名为 `explore`、本轮没有 `inputResponses`、有效协议版本不早于 `2026-07-28`、客户端声明 `elicitation.form`，并且完整结果是现有 `ambiguous: true` 候选响应时，适配层将它转换为 `InputRequiredResult`。

候选判断以现有公开结果中的 `candidates[].symbol_ref` 为单一事实源，避免在适配层复制 scoped resolution、排序和候选上限。其他结果继续直接转换成 `CallToolResult`。

备选方案是在核心路由中引入 rmcp 类型或重构所有工具为新的 typed outcome；这会扩大公共 API 和回归面，与当前单工具切片不相称，因此不采用。

### 2. 使用一个表单 elicitation 枚举表达候选

`inputRequests` 包含一个固定标识的表单 elicitation。表单只有一个必填字符串字段；每个枚举值是候选 `symbol_ref` 的紧凑 JSON 字符串，显示标题使用候选限定名、文件和行号。这样既符合 elicitation 仅允许原始属性类型的约束，也能无损携带现有精确选择器。

候选值来自 Atlas 已经返回的有界列表，不增加新的结果规模上限。

### 3. 接受响应后复用普通 SymbolSelector 路径

重试中，适配层读取固定输入响应：

- `accept` 必须携带字符串选择值；该值必须能解析为 JSON 对象。由于本流程不保存服务器状态，客户端重试 MUST 重发原始参数对象；适配层在该对象中替换 `symbol`，原请求其余参数不变，然后只执行一次正常 `explore`。缺失参数对象时返回 `invalid_params`，不得静默构造一个只含 `symbol` 的新调用。
- `decline` 或 `cancel` 不改写参数，并禁止本轮再次转换为 `input_required`，因此返回现有候选完整结果而不是形成循环。
- 缺失、未知或畸形响应返回 JSON-RPC `invalid_params`，不猜测候选。

选择值仍是不可信用户输入，并经过现有 `parse_symbol_input`、字段反序列化、路径与长度校验。该无状态设计有意不把一次重试绑定到先前某个候选集合或并发 offer；客户端即使提交枚举外或来自另一轮 offer 的有效 `SymbolSelector`，系统也只把它当作普通 `explore` 用户输入。这与直接发起同一调用具有相同权限和行为，选择值 MUST NOT 影响授权、项目身份或其他资源。新增回归测试固定这一边界，避免未来误把无状态候选值当成可信服务器断言。

### 4. 本切片不使用 requestState

`InputRequiredResult` 只携带 `inputRequests`，明确省略 `requestState`。候选选择不需要保存服务器进度：原始工具参数会由客户端在重试中保留，选择值自身就是完整的精确输入。Atlas 对该 `explore` 流程收到的非空 `requestState` 返回 `invalid_params`，不会解析或信任它。

备选方案是签名并设置 TTL 的 `requestState`。它需要进程密钥、重放策略和额外依赖，却没有为这个无状态单轮选择提供必要安全收益，因此暂不采用；未来若状态会影响授权、资源访问或业务逻辑，必须另立变更并使用完整的完整性、过期和重放防护。

### 5. 能力与兼容门控采用同一个有效请求上下文

协议版本使用 `RequestContext::protocol_version()`，客户端能力使用 `RequestContext::client_capabilities()`；只有版本是格式有效且不早于 `2026-07-28` 的 ISO `YYYY-MM-DD` 日期、并且 `elicitation.form` 存在时启用。未知、缺失、非日期、旧版本、缺失客户端能力或仅 URL elicitation 都保留现有完整候选结果。rmcp 自身会拒绝未受支持的请求版本，并以同一请求上下文检查 SEP-2322 结果，作为外层防线。

## Risks / Trade-offs

- [适配层解析自身 JSON 结果会耦合现有歧义响应形状] → 只读取 `ambiguous` 与 `candidates[].symbol_ref`，并用现有核心回归及新增转换测试锁定；不改变核心响应。
- [恶意客户端可提交枚举外选择] → 将其视为普通 `SymbolSelector` 输入并走同一校验/权限边界，不将候选值用于授权或项目切换。
- [用户拒绝后仍需要看到候选] → 明确回退到当前完整候选响应，并抑制同一重试再次进入 MRTR。
- [支持现代协议但没有交互 UI 的客户端可能无法处理输入请求] → 额外要求客户端声明表单 elicitation 能力，否则不启用。
- [未来需要服务器状态时无 `requestState` 可扩展性] → 通过独立 OpenSpec 引入签名、TTL 和重放策略，不提前加入未使用的状态机制。

## Migration Plan

1. 先加入纯适配 helper 与单元测试，锁定能力门控、候选请求 wire shape、接受/拒绝/畸形重试。
2. 将 helper 接入 `ServerHandler::call_tool()`，运行完整 MCP/CLI 回归，确认核心路由与旧协议结果未改变。
3. 如需回滚，只移除适配层 MRTR 分支；工具 schema、核心结果和持久化数据无需迁移。