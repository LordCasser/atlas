## Context

`ToolRouter` 在构造时通过 `make_all_tools()` 固定 15 项工具目录，目录不依赖 active project、用户身份或授权状态。MCP transport 适配层当前直接把该目录转换为 `rmcp::model::ListToolsResult`，但忽略 `RequestContext` 中的协商版本。

锁定的 `rmcp 3.0.1` 已提供 `RequestContext::protocol_version()`、`ListToolsResult::with_ttl_ms()`、`with_cache_scope()` 与 `CacheScope::Public`。SDK 会为旧协议去掉 `resultType`，但不会自动替 Atlas 去掉业务 handler 设置的缓存字段。

## Goals / Non-Goals

**Goals:**

- 由 MCP transport 适配层统一拥有协议版本分支。
- 为现代协议提供正 TTL 和 authorization-independent 的公共缓存范围。
- 用序列化结果验证现代与旧协议的实际 JSON 字段形状。

**Non-Goals:**

- 不改变工具注册、schema、顺序或 dispatch。
- 不实现服务端工具目录缓存；缓存消费由 MCP 客户端/中间层负责。
- 不升级 `rmcp`，不引入授权系统，也不处理动态工具目录。

## Decisions

### 1. 协议分支留在 `AtlasMcpService`

`ToolRouter` 继续返回 transport-agnostic 的 Atlas `ListToolsResult`。`AtlasMcpService::list_tools` 从 `RequestContext::protocol_version()` 读取最终协商版本，并在转换后的 rmcp result 上附加协议元数据。

这样保持依赖方向为 `router → Atlas contract`、`transport adapter → rmcp wire contract`，避免把 MCP 版本知识泄漏进工具注册层。

**Alternative:** 在 `ToolRouter::list_tools()` 增加版本参数。该方案会污染目前可被测试和 TUI 复用的 transport-neutral 边界，因此不采用。

### 2. 使用 300,000 ms 的公共 TTL

工具目录在一个 Atlas 二进制版本内稳定，且不包含授权相关差异，因此使用 `cacheScope: public`。TTL 取 5 分钟：足以避免短时间内重复传输完整 schema，同时对二进制升级或未来目录策略变化保持保守的陈旧窗口。

TTL 定义为单一命名常量并保证大于零。若未来工具目录按项目或身份变化，必须重新评估 `public`，而不是在外围增加例外。

### 3. 未知版本按 legacy-safe 处理

仅当版本字符串为 `2026-07-28` 或更新的日期版本时添加缓存字段。`None`、`2025-11-25` 及更旧版本均省略字段。日期版本比较沿用 rmcp 对 ISO `YYYY-MM-DD` 协议版本的有序比较方式。

### 4. 测试锁定 rmcp 序列化后的 wire shape

测试通过 Atlas 的版本感知结果构造边界生成现代与旧结果，序列化为 JSON 并断言：

- 现代结果包含正 `ttlMs`、`cacheScope: "public"` 和完整工具数组；
- 旧结果省略缓存字段，并经 rmcp legacy 处理后省略现代 `resultType`；
- 两侧 `tools` 数组完全一致。

这比只断言 Rust 字段赋值更接近实际 wire contract，同时不需要启动 stdio 子进程。

## Risks / Trade-offs

- [未来目录变为按身份动态] → `public` 会造成越权缓存语义；当前实现旁增加不变量注释，并由 catalog parity 测试固定当前静态行为。
- [过长 TTL 延迟观察新工具] → 使用 5 分钟保守 TTL；二进制重启和客户端重新协商仍会获取新目录。
- [只测结构序列化，未覆盖 framing] → 本次字段属于 JSON result 语义，rmcp 已拥有 framing；Atlas 测试覆盖其实际 model 和 legacy strip 路径，现有 SDK 协议版本测试继续覆盖协商支持。

## Migration Plan

无需数据迁移。发布后现代客户端可选择使用缓存提示，旧客户端收到与此前相同的字段形状。回滚只需移除两个缓存字段的条件设置，不影响工具调用或持久化状态。
