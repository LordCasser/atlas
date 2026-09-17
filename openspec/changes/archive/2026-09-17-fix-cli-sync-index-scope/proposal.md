## Why

`atlas index --include/--scope/--exclude` 会把筛选条件持久化到 `project_metadata.indexed_scope`，但 `atlas sync` 的两个 change-detection 入口始终使用 `DiscoveryConfig::default()`。已通过真实 CLI binary 与磁盘 SQLite 复现：先以 `--scope src` 建库，随后执行默认 sync，原 scope 外的 `other/dep.ts` 被加入数据库，而 `indexed_scope` 仍宣称只有 `src/**`。

这使持久事实集合与其范围 authority 不一致。scope 外源码可能被解析、解析关系和 summary，既破坏用户声明的范围，也让 status、查询和后续增量行为无法从 metadata 判断数据库真实边界。

## What Changes

- 让 filesync 的普通与 capability-aware change detection 从 Store 读取并应用持久 `indexed_scope`，而不是默认扫描全项目。
- 对当前规范形状的 include/exclude object 做显式校验；metadata 缺失时保留 legacy whole-project 兼容，存在但损坏时 fail closed。
- 让 scope 内 add/modify/delete 与 capability upgrade 继续复用既有 dirty-set；已被旧缺陷写入的 scope 外缓存 facts 在同步时按删除路径收敛，源文件不受影响。
- 增加 engine 级检测/状态转换回归与真实 CLI binary → 持久 SQLite 回归，覆盖 include、exclude、scope 内删除、scope 外隔离、capability upgrade 与 metadata 稳定性。
- 保持 CLI schema、Focus/MCP request scope、discovery 基础策略、precision guard、path-alias、stdout 和数据库 schema 不变。

## Capabilities

### New Capabilities

- `cli-sync-index-scope`: 定义 `atlas sync` 继承最后一次成功 Index 的持久 source-discovery scope，并使数据库事实与该范围重新收敛。

### Modified Capabilities

<!-- 无。 -->

## Impact

- 影响 `crates/atlas-engine/crates/filesync` 的 discovery config 选择、change detection 和相关回归。
- 增加 `atlas-cli` 真实进程测试，但不为 sync 新增 include/scope/exclude 参数。
- 对已被旧行为污染的数据库，首次成功 sync 会移除 scope 外索引缓存；不会删除或修改工作区源码。
- 验收使用 `CARGO_BUILD_JOBS=1` 与 `--test-threads=1`，禁止并发 Cargo 编译；切片完成后执行 `cargo clean`。
