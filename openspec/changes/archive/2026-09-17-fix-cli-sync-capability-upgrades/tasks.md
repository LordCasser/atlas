## 1. 共享 dirty-set 与提交语义

- [x] 1.1 增加 mode-aware incremental change detection，复用 `build_dirty_set_for_mode` 并区分 existing reindex、added 与 deleted 路径
- [x] 1.2 让 `IncrementalPipeline` 以目标 capability 选择 workset，保持内容变化、alias change 与同等级 no-op 语义
- [x] 1.3 在成功 ConfigCommit 中提交目标 `indexed_pipeline_grade`，确保失败或中断不提前升级 metadata
- [x] 1.4 传播 dirty-set Store authority 错误，不得用空集合掩盖 DB 读取失败

## 2. 可判别回归

- [x] 2.1 增加 engine 回归，覆盖 hash-clean `manifest → structural` 与 `structural → full`，验证 reindex、layers、graph/summary、grade 和 `last_sync_time`
- [x] 2.2 覆盖目标 capability 已满足时的同等级 no-op，确认不重复抽取且事实稳定
- [x] 2.3 增加真实 CLI binary → 持久 SQLite 回归；每步跨进程重开 DB，且不依赖 stdout/TTY/中断时序
- [x] 2.4 保持 precision downgrade、内容增删改、path-alias 与既有 pipeline equivalence 回归通过

## 3. 文档与验收

- [x] 3.1 更新路线图与变更日志，记录 CLI sync capability upgrade 的权威边界与真实进程回归
- [x] 3.2 串行运行严格 OpenSpec、格式、filesync、`atlas-cli` 与全目标 `-D warnings` Clippy 验收
- [x] 3.3 完成独立审查并修复有效发现
- [x] 3.4 归档后运行全部 OpenSpec 严格校验、`git diff --check`、`cargo clean`、提交并推送
