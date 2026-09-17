## Why

`atlas sync` 当前只比较磁盘内容哈希。若项目先以 `manifest` 或 `structural` 建库，再在源码完全未变化时请求更高分析等级，`IncrementalPipeline` 会在 change detection 后直接返回：命令报告成功，但不会重抽取、不会生成 graph/summary，也不会更新 `indexed_pipeline_grade` 或 `last_sync_time`。

这违反共享索引管线已经定义的 clean 判定：文件只有在“content hash 相同且 fresh complete capability 满足目标 `ExtractionMode`”时才是 clean。已通过真实 CLI binary 与持久 SQLite 分别复现 `manifest → structural` 和 `structural → full` 静默 no-op，因此不是测试覆盖不足，而是入口行为与权威 dirty-set 契约漂移。

## What Changes

- 让 incremental sync 复用 capability-aware dirty-set 边界；哈希未变但缺目标 capability 的已索引文件进入重抽取 workset。
- 在成功完成升级所需的 extraction、resolution、graph 和 summary 阶段后，提交对应 `indexed_pipeline_grade` 与既有 sync metadata。
- 增加 engine 级状态转换回归和真实 CLI binary → 持久 SQLite 回归，覆盖 `manifest → structural`、`structural → full` 以及同等级无变化 no-op。
- 保持内容变更、新增、删除、path-alias invalidation、precision downgrade guard、进度协议、CLI 参数和 stdout 非契约行为不变。

## Capabilities

### New Capabilities

- `cli-sync-capability-upgrades`: 定义 `atlas sync --analysis <higher-grade>` 在源码哈希未变化时仍按目标 capability 收敛持久索引的行为。

### Modified Capabilities

<!-- 无。 -->

## Impact

- 影响 `crates/atlas-engine/crates/filesync` 的 incremental change detection、dirty workset 与成功提交 metadata。
- 增加 `atlas-cli` 的真实 binary 持久 DB 回归，但不改变 CLI schema、输出格式或 TUI/中断控制面。
- 不涉及 lazy Focus、MCP、graph cancellation、数据库 schema 或同步分析预算。
- 验收继续使用 `CARGO_BUILD_JOBS=1` 与 `--test-threads=1`，避免并行编译造成 OOM；切片完成后执行 `cargo clean`。
