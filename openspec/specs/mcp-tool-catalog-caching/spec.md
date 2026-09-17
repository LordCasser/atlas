# mcp-tool-catalog-caching Specification

## Purpose
为 Atlas 的确定性 MCP 工具目录定义版本感知的标准缓存提示，使现代客户端可以安全复用目录，同时保持旧协议 wire shape 与既有工具契约不变。

## Requirements

### Requirement: Modern protocol exposes cache metadata
当 `tools/list` 请求协商的 MCP 协议版本为 `2026-07-28` 或更新版本时，服务端 MUST 在结果中返回正整数 `ttlMs`，并 MUST 返回 `cacheScope: "public"`。

#### Scenario: Current protocol requests the tool catalog
- **WHEN** 客户端以 MCP `2026-07-28` 请求 `tools/list`
- **THEN** 响应包含大于零的 `ttlMs`
- **AND** 响应包含值为 `public` 的 `cacheScope`
- **AND** 响应继续包含完整工具目录

#### Scenario: A newer date-version requests the tool catalog
- **WHEN** 服务端已成功协商一个受支持且晚于 `2026-07-28` 的日期版本，客户端请求 `tools/list`
- **THEN** 服务端按现代协议返回相同的缓存元数据

### Requirement: Legacy protocol omits cache metadata
当协商版本早于 `2026-07-28` 或无法确定协议版本时，服务端 MUST 省略 `ttlMs` 与 `cacheScope`，不得以 `null` 或零值代替省略。

#### Scenario: Legacy protocol requests the tool catalog
- **WHEN** 客户端以 MCP `2025-11-25` 请求 `tools/list`
- **THEN** 响应不包含 `ttlMs`
- **AND** 响应不包含 `cacheScope`
- **AND** 响应保持旧协议所要求的结果字段形状

#### Scenario: Protocol version is unavailable
- **WHEN** 服务端无法从请求或已协商会话确定协议版本
- **THEN** 响应采用旧协议兼容形状并省略缓存元数据

### Requirement: Cache metadata does not alter the catalog contract
缓存元数据 MUST 只描述 `tools/list` 结果的新鲜度和共享范围，不得改变工具名称、顺序、描述、输入 schema 或 `tools/call` 行为。

#### Scenario: Compare modern and legacy catalogs
- **WHEN** 对同一 Atlas 二进制分别构造现代和旧协议的 `tools/list` 响应
- **THEN** 两个响应的 `tools` 数组完全相同
- **AND** 差异仅来自协议版本允许或要求的结果元数据
