## Why

Atlas 的 15 项 MCP 工具目录在同一二进制版本内是确定且与授权身份无关的，但当前 `tools/list` 每次都完整传输目录，未利用 MCP `2026-07-28` 已提供的结果缓存语义。`rmcp 3.0.1` 已暴露 `ttlMs`、`cacheScope` 和请求协议版本，因此可以在不改变工具契约的前提下完成路线图中的协议演进项。

## What Changes

- 对协商为 MCP `2026-07-28` 或更新版本的 `tools/list` 响应附加正值 `ttlMs` 与 `cacheScope: "public"`。
- 对 MCP `2025-11-25`、缺失版本信息或更旧版本继续省略这两个字段。
- 增加序列化/wire-shape 回归，锁定现代协议元数据、旧协议省略及两边工具目录一致性。
- 更新路线图和 Unreleased changelog，记录该项已完成。

## Capabilities

### New Capabilities

- `mcp-tool-catalog-caching`: 定义确定性 MCP 工具目录的版本感知缓存元数据和旧协议兼容行为。

### Modified Capabilities

- 无。

## Impact

- 代码：`crates/atlas-mcp/src/lib.rs` 的 `ServerHandler::list_tools` 适配层。
- 测试：`atlas-mcp` 的协议版本与 JSON wire shape 回归。
- 文档：`docs/roadmap.md` 与 `CHANGELOG.md`。
- 依赖：不新增或升级依赖；继续使用锁定的 `rmcp 3.0.1`。
- 公共契约：15 个工具名称、顺序、输入 schema 和 `tools/call` 行为保持不变；仅现代协议响应新增标准字段。