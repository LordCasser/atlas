## Context

共享 full-index 路径已经通过 `build_dirty_set_for_mode` 把 clean 定义为：磁盘 raw-content hash 与 `files.content_hash` 相同，并且当前 fresh complete file-level extraction state 覆盖目标 `ExtractionMode`。`IndexPipeline` 因此能在文件未修改时完成 `manifest → structural → full` 升级。

`IncrementalPipeline` 仍调用只比较 hash 的 `detect_changes()`。它在 `changed.is_empty() && !alias_changed` 时立即成功返回，所以缺 capability 的 hash-clean 文件永远不会进入 cleanup/extraction workset。真实 CLI 复现结果为：

- `manifest → sync structural` 后 grade 仍为 `manifest`，只有 manifest layer，graph edge 为 0，`last_sync_time` 缺失；
- `structural → sync full` 后 grade 仍为 `structural`，没有 dataflow layer、function summary 或 `last_sync_time`；
- 两次命令均以成功状态退出并显示 0 files reindexed。

入口层不应自行扫描 capability。修复必须位于 filesync 的共享 change-detection/dirty 边界，并让 CLI 只继续负责 mode 解析、precision guard、锁、线程和进度。

## Goals / Non-Goals

**Goals:**

- 让 incremental sync 在 hash-clean 文件缺目标 capability 时重抽取这些文件。
- 让成功升级完整经过目标 mode 的 resolution/graph/summary 阶段并提交 grade。
- 用 engine 级与真实 CLI process 级回归证明持久 DB 从低等级收敛到高等级。
- 保持同等级 hash-clean sync 的 no-op 行为，以及既有内容变更、新增、删除和 alias-change 语义。

**Non-Goals:**

- 不为 `sync` 新增 include/scope/exclude 参数，也不重定义已有 scoped-index 后续 sync 行为。
- 不改变 `--force-reindex` 或 precision downgrade 的产品语义。
- 不修改 extraction、resolution、graph 或 summary 算法。
- 不引入新的数据库表、迁移、MCP/Focus 控制面或 CLI 输出快照契约。
- 不在本切片增加 graph phase 内部 cancellation checkpoint。

## Decisions

### 1. incremental change detection 复用 capability-aware dirty set

新增 mode-aware detection 路径：先使用现有 canonical discovery，再调用 `build_dirty_set_for_mode`。`DirtySet::dirty` 中原本已存在于 Store 的路径归入 reindex/modified workset；尚不存在的路径归入 added；`DirtySet::deleted` 保持删除语义。

这让 hash、path normalization、fresh extraction state 和 capability mapping 继续只有一个权威判定。普通 `detect_changes` 的内容差异 API 保持兼容；`IncrementalPipeline` 改用 mode-aware 入口。

### 2. capability dirty 文件走完整既有增量阶段

缺 capability 的已索引文件与内容修改文件一样，先清理该文件的旧 facts，再按目标 mode 重抽取。后续继续复用现有条件：

- `Structural` 执行 reference resolution、graph build 与 annotation materialization；
- `Full` 额外执行 dataflow extraction 和 summary build；
- 升级 workset 覆盖全项目时，既有 ≥30% 规则自然选择 full summary rebuild。

不创建独立“补 layer”旁路，避免旧 facts、resolution fingerprint、graph edge 或 summary capability 与新 mode 不一致。

### 3. 成功提交同步目标 grade

`ConfigCommit` 在 path-alias baseline 成功提交后写入目标 `indexed_pipeline_grade`，再按现有顺序写 `last_sync_time`。中断或任一前置阶段失败时不提升 grade；这样 metadata 只声明已经完成的 pipeline。

同等级且 capability 已满足的无变化同步仍在早期返回，不为了本变更产生额外 extraction、resolution 或 summary 工作。

### 4. engine 与真实 CLI 使用持久事实作为 oracle

engine 回归直接构造低等级 index 后以同一 Store 运行 `IncrementalPipeline`，验证 files reindexed、extraction layers、graph/summary、grade 与 no-op 稳定性。

CLI 回归启动 Cargo 构建的真实 `atlas` binary，每一步在独立进程完成，并重新打开 `.atlas/atlas.db` 检查稳定事实。测试只要求退出状态和 DB state，不比较 spinner、phase timing、stdout 排版或 Ctrl+C 时序。

## Risks / Trade-offs

- [升级时会重抽取所有缺 capability 文件] → 这是目标 mode 的必要工作；现有 dirty-set 和并行 extraction 控制成本，不提高预算。
- [把 capability workset 计入 modified/files_changed] → 这些文件确实需要重建持久 facts；CLI 用户更关心 `files_reindexed`，不新增公开 stats 字段。
- [成功 sync 更新 grade] → 只在全部既有阶段成功后提交；失败/中断保持旧 grade，避免虚假权威。
- [真实 binary 回归较慢] → fixture 仅含两个 TypeScript 文件，并串行复用同一 binary，不做 stdout snapshot 或大型三模式矩阵。
- [dirty-set Store 读取失败被误作空库] → mode-aware 路径必须传播 `list_files` 错误，不得把 DB 故障解释为全量新增。

## Migration Plan

1. 增加 mode-aware change detection，并让 `IncrementalPipeline` 使用它。
2. 在 sync 成功提交阶段记录目标 pipeline grade。
3. 增加 engine 与真实 CLI 持久 DB 状态转换回归。
4. 串行运行严格 OpenSpec、filesync/CLI 测试与 Clippy。
5. 独立审查后归档；回滚只需恢复 hash-only detection 与 grade 提交改动，不涉及数据迁移。
