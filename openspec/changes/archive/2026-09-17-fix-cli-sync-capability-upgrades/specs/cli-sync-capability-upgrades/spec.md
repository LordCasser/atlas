## ADDED Requirements

### Requirement: sync clean 判定必须包含目标 capability

系统 MUST 仅在源文件 raw-content hash 与持久 `files.content_hash` 相同，且该文件具有满足本次 `ExtractionMode` 的 fresh complete file-level capability 时，将文件视为 sync clean。系统 SHALL 复用 filesync 的 capability-aware dirty-set 权威，不得在 CLI 入口复制 capability 判断。

#### Scenario: manifest 文件升级为 structural

- **GIVEN** 一个 whole-project 持久索引仅具有 fresh complete manifest facts
- **WHEN** 用户在源码未变化时执行 `atlas sync --analysis structural`
- **THEN** 所有缺 structural capability 的文件进入 reindex workset，并完成 structural extraction、resolution 与 graph build

#### Scenario: structural 文件升级为 full

- **GIVEN** 一个 whole-project 持久索引仅具有 fresh complete structural facts
- **WHEN** 用户在源码未变化时执行 `atlas sync --analysis full`
- **THEN** 所有缺 dataflow capability 的文件进入 reindex workset，并完成 full extraction 与 function summary build

#### Scenario: 更高 capability 满足较低请求

- **GIVEN** 文件已有 fresh complete dataflow capability 且 hash 未变化
- **WHEN** shared dirty-set 以 structural 要求检查该文件
- **THEN** 系统 SHALL 将该文件视为 clean，不得仅因 layer 名称不同而重复抽取

### Requirement: capability 升级必须走既有完整增量管线

系统 MUST 对 capability-dirty 文件复用内容修改文件的 cleanup、extraction、DB write 及目标 mode 后续阶段。系统不得通过只改 metadata、补写 extraction-state 或按结果数量推断完成度来伪造升级。

#### Scenario: structural 升级重建旧 facts

- **WHEN** hash-clean manifest 文件因 structural capability 缺失进入 workset
- **THEN** 系统先清理该文件的旧 facts，再写入当前源码的 structural facts，并由现有 resolution/graph 阶段建立持久关系

#### Scenario: full 升级产生 summary capability

- **WHEN** hash-clean structural 文件因 dataflow capability 缺失进入 workset
- **THEN** 系统通过既有 Full extraction 与 summary builder 生成持久事实，并记录与当前 content hash 一致的 capability state

#### Scenario: 升级阶段失败

- **WHEN** extraction、DB write、resolution、graph、summary 或 config commit 任一必要阶段失败
- **THEN** sync MUST 返回错误，且不得把 `indexed_pipeline_grade` 提升为目标等级

### Requirement: 成功升级必须提交可验证的持久终态

系统 MUST 在 capability upgrade 成功后写入目标 `indexed_pipeline_grade`，并按既有提交顺序写入 `last_sync_time`。最终 grade、file-level extraction state、graph/summary facts 必须相互一致。

#### Scenario: manifest 到 structural 成功收敛

- **WHEN** `atlas sync --analysis structural` 成功升级一个 manifest-only 项目
- **THEN** 持久 DB 的 grade 为 `structural`，每个当前索引文件具有 fresh complete structural capability，目标 fixture 的 graph facts 可见，且 `last_sync_time` 存在

#### Scenario: structural 到 full 成功收敛

- **WHEN** `atlas sync --analysis full` 成功升级一个 structural-only 项目
- **THEN** 持久 DB 的 grade 为 `full`，每个当前索引文件具有 fresh complete dataflow capability，需要 summary 的函数具有持久 summary，且 `last_sync_time` 存在

#### Scenario: 中断不提前发布 grade

- **WHEN** sync 在最终 metadata commit 前被中断
- **THEN** 系统不得写入尚未完成的目标 grade

### Requirement: 既有 sync 兼容边界必须保持

系统 MUST 保持 CLI 命令与参数、precision downgrade guard、内容修改/新增/删除、path-alias invalidation、进度事件、文件锁和同等级 no-op 语义不变。stdout、spinner 文本与 phase duration 不属于本能力的稳定契约。

#### Scenario: 同等级无变化同步保持 no-op

- **GIVEN** 所有文件的 hash 与目标 capability 均已满足
- **WHEN** 用户以相同分析等级再次执行 sync
- **THEN** 系统不得重新抽取文件，并保持持久事实集合稳定

#### Scenario: 默认拒绝精度降级

- **GIVEN** 现有索引 grade 高于请求 mode
- **WHEN** 用户未提供 `--force-reindex` 执行 `atlas sync`
- **THEN** 既有 precision downgrade guard 继续拒绝操作，且 DB 不被修改

#### Scenario: DB authority 读取失败

- **WHEN** capability-aware dirty-set 无法读取 Store 中的已索引文件或 extraction state
- **THEN** 系统 MUST 传播错误，不得把故障解释为空库、全量新增或成功 no-op

### Requirement: CLI 回归必须验证真实进程与持久 DB

系统 SHALL 通过真实 `atlas` binary 和磁盘 SQLite 验证至少一次 `manifest → structural → full` 的无源码变化状态转换。测试 MUST 以进程退出状态和重新打开后的 DB facts 为主要 oracle，不得依赖完整 stdout/stderr、TTY spinner 或 Ctrl+C 时序。

#### Scenario: 每次命令跨进程重开 DB

- **WHEN** 回归依次运行 index 与两次 sync
- **THEN** 每个命令都作为独立进程退出，测试重新打开 `.atlas/atlas.db` 验证 grade、layers、graph/summary 和 sync metadata

#### Scenario: 输出样式变化

- **WHEN** spinner、phase timing 或非契约输出格式发生变化，但退出状态和 DB 终态仍正确
- **THEN** capability upgrade 回归继续通过
