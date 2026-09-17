## Context

Index discovery 已以 `IndexPipelineOptions.include_patterns/exclude_patterns` 为权威，并在 finalize 时持久化：

```json
{"include":["src/**"],"exclude":["generated/**"]}
```

Sync CLI 没有范围参数；它应延续这份最后成功 Index 的范围。当前 `detect_changes()` 与 `detect_changes_for_mode()` 都重新使用空 `DiscoveryConfig`，导致默认全项目 discovery。真实进程复现中，scope 外文件由 `added → extraction → DB write` 进入索引，sync 仍只更新 grade/time，旧 `indexed_scope` 保持不变。

修复应位于 filesync 共享 detection 边界，而不是 CLI 参数层。scope 只限制 source discovery；path alias 配置、跨文件 resolution、summary 和 metadata 提交继续复用既有管线。

## Goals / Non-Goals

**Goals:**

- 让所有增量 change detection 继承合法的持久 `indexed_scope`。
- 保持 scope 内内容 add/modify/delete、hash-clean capability upgrade 与同等级 no-op。
- 对损坏 scope fail closed，避免不确定 metadata 静默扩大为全项目扫描。
- 让既有 scope 外缓存 facts 通过普通删除清理收敛，使 DB inventory 与 metadata 一致。
- 用 engine 与真实 CLI 跨进程测试锁定持久 DB 终态。

**Non-Goals:**

- 不给 `atlas sync` 新增 `--include`、`--scope` 或 `--exclude`。
- 不修改 `git ls-files`、filesystem walk、`.atlasignore` 或默认目录排除算法。
- 不改变 Focus/MCP 的 request-scoped `include_roots` 或 materialization 控制面。
- 不改变 extraction、resolution、graph、summary、precision downgrade 或 path-alias 算法。
- 不增加数据库 schema、scope 迁移命令或源码删除行为。
- 不处理 graph edge-building cancellation；该项继续作为独立后续切片。

## Decisions

### 1. 在 filesync detection 边界加载持久 scope

新增单一 helper，把 `project_metadata.indexed_scope` 转换为 `DiscoveryConfig`。`detect_changes()` 与 `detect_changes_for_mode()` 都调用该 helper，再把得到的 config 交给现有 `discover_files()`。

CLI 继续只负责 mode、锁、线程和进度；不在 CLI 层解析 metadata，也不复制 glob 判定。capability-aware detection 仍继续调用 `build_dirty_set_for_mode`，只改变它接收的 discovered source 集合。

### 2. 当前格式严格、legacy missing 兼容

合法 metadata MUST 是 JSON object，且同时包含 string-array 类型的 `include` 与 `exclude`。未知额外字段可忽略，以保留未来扩展空间；数组元素沿用 Index 已接受的原始 pattern，不在本切片新增空字符串或 glob 语法校验。

- key 缺失：视为旧版 whole-project 索引，返回默认 config，并记录 tracing warning；这样旧数据库仍可 sync。
- key 存在且 `{ "include": [], "exclude": [] }`：明确的 whole-project scope。
- key 存在但 JSON、顶层类型、字段或元素类型错误：返回带 metadata 上下文的错误，不运行 cleanup/extraction，也不提交 sync metadata。

不猜测 `[]` 等未知旧格式，因为现有 authority 判定同样不把这些形状视为合法 whole-project scope。

### 3. scoped discovered set 继续作为 inventory authority

Dirty-set 的集合差语义保持不变：

- scope 内磁盘新增文件进入 added；
- scope 内 hash 变化或 capability 不足文件进入 modified/reindex；
- Store 中存在、但不再属于 scoped discovered set 的路径进入 deleted。

最后一条既处理 scope 内源码删除，也修复旧缺陷留下的 scope 外缓存污染。清理只作用于 `.atlas/atlas.db` 中可再生 facts，不删除工作区文件；CLI 现有 `files_removed` 统计提供可见结果。

### 4. scope metadata 保持稳定

Sync 不重新生成或覆盖 `indexed_scope`。合法 scope 是本次 discovery 的输入，也是成功后的范围声明；现有原子 `commit_sync_metadata` 只提交目标 grade 与 `last_sync_time`。

同等级且 scoped inventory/capability 均已收敛时继续早期 no-op，不刷新 sync timestamp。发生 malformed scope、extraction、DB write、resolution、summary 或 metadata 错误时，不发布新的成功终态。

### 5. 以持久 SQLite 而非输出文本作为测试 oracle

Engine 回归覆盖 config 解析及 scoped dirty-set，状态转换回归覆盖 scope 内 add/modify/delete、外部隔离和 capability upgrade。

CLI 回归启动真实 `atlas` binary：scope index 后在范围内外制造变化，逐次运行 sync 并重新打开 `.atlas/atlas.db`，验证 files、extraction layers、grade、`indexed_scope` 与 `last_sync_time`。测试不依赖 spinner、stdout 排版、phase timing 或 Ctrl+C 时序。

## Risks / Trade-offs

- [旧污染 DB 首次 sync 会报告并移除 scope 外索引文件] → 这是恢复 metadata/事实不变量的必要 reconciliation；只删缓存 facts，不碰源码。
- [missing scope 回退全项目可能掩盖人为 metadata 删除] → 为旧数据库兼容保留 fallback，并记录 warning；存在但损坏的值一律 fail closed。
- [同一 metadata 被两个 detector 解析] → helper 单一实现，两个入口复用，避免未来 hash-only caller 再次越界。
- [scope pattern 语义可能演进] → helper 只负责当前结构和类型；匹配仍由 canonical discovery 实现，不复制 glob 引擎。
- [真实 CLI 回归增加构建时间] → 使用小型 TypeScript fixture，并继续串行低内存执行。

## Migration Plan

1. 增加 `indexed_scope` → `DiscoveryConfig` helper 与错误语义单测。
2. 让普通及 mode-aware detector 使用持久 scope。
3. 增加 scoped engine 状态转换与真实 CLI 持久 DB 回归。
4. 更新路线图和变更日志，串行运行严格 OpenSpec、filesync/CLI 测试与 Clippy。
5. 独立审查后归档、全局严格校验、`cargo clean`、提交并推送；回滚只恢复 discovery config 选择，不涉及 DB migration。
