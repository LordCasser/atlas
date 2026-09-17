## ADDED Requirements

### Requirement: cold search 终态必须与后台物化的当前事实收敛

系统 MUST 在 scoped `search` 首次返回 partial 且携带 deferred file IDs、但后台 Focus 协调时已无 pending work 的情况下，使用同一请求至多重读一次当前 Store。系统 SHALL 仅在重读证明 scope structural coverage 完整时发布 `coverage.state: "complete"`；不得用命中数量猜测完整性。

#### Scenario: 后台在搜索计数后完成

- **WHEN** 重放先读取到 scope 仍有 inventory-only 文件，而后台 Focus 在本轮结果发布前完成这些文件，且协调时已无 pending work
- **THEN** 系统重读同一 search 一次，并基于当前 Store 返回结果与 coverage

#### Scenario: 全部命中已可见但旧标记仍为 partial

- **WHEN** 第一次执行的结果已包含 scope 中全部匹配项，但它沿用了后台提交前计算的 bounded/inventory 标记
- **THEN** 系统不得仅因旧标记发布 terminal partial；单次重读证明完整时必须返回 complete

#### Scenario: 不按结果数量推断完整性

- **WHEN** 当前结果数量等于 inventory 文件数，但仍有文件缺少 fresh complete structural facts
- **THEN** 系统 MUST 保持 partial，并按现有 pending 或 terminal gap 规则表达不完整性

#### Scenario: 入队失败不是完成证据

- **WHEN** deferred Focus 入队返回错误
- **THEN** 系统 MUST 进入现有 search 错误路径，不得把该错误解释为 zero-pending 或 complete

### Requirement: 单次协调不得形成新的恢复循环

系统 MUST 将竞态协调限制为至多一次额外 `ScopedSearchService` 执行。系统 MUST 使用与协调决策同一次 tracker 观察所得的 pending 数量和 ETA 构造本轮响应，不得因作业随后完成而把旧 partial 发布为无 retry 终态。重读后若仍有真实 pending work，系统 SHALL 保持现有 `query_id` 与正整数 `analysis.retry_after_ms`；若无 pending 且仍无法证明完整，系统 SHALL 返回现有 terminal partial gap，不得继续自旋或创建新的控制面。

#### Scenario: 重读发现仍有待处理作业

- **WHEN** 单次重读产生 deferred files，且现有 Focus 调度器返回正数 pending work
- **THEN** 响应继续是 retryable，并由现有 `resume_query` 或 MCP task 路径等待

#### Scenario: 作业在 pending 观察后立即完成

- **WHEN** 协调读取到正数 pending work，而该作业在最终 JSON 构造前完成
- **THEN** 本轮仍按该一致观察返回 retry marker，下一次恢复收敛；不得返回 stale terminal partial

#### Scenario: 重读只剩永久边界或失败

- **WHEN** 单次重读仍为 partial，但调度器无 pending work
- **THEN** 响应是无 retry marker 的 terminal partial，并保留现有结构化 gap/失败语义

#### Scenario: 普通完整搜索不额外执行

- **WHEN** 第一次 scoped search 已返回 complete，或没有 deferred file IDs
- **THEN** 系统不得为本变更增加第二次搜索执行

### Requirement: complete 响应不得保留过时的不完整提示

系统 MUST 让最终 `coverage`、`analysis`、`warnings`、`gaps` 与协调后的搜索结果一致。重读变为 complete 时，不得保留仅由第一次 bounded pass 产生的 retry marker、closure-boundary gap 或“需缩小 scope 才能精确”的过时提示。

#### Scenario: 竞态协调后完整

- **WHEN** 单次重读返回 full structural coverage
- **THEN** 最终响应包含 `coverage.state: "complete"`、全部当前命中且不含 `analysis.retry_after_ms` 或 `closure_boundary` gap

### Requirement: 兼容边界必须保持不变

系统 MUST 保持 search 输入 schema、结果排序与 limit、15 项工具目录、Focus scheduler 预算、query snapshot TTL、Tasks/MRTR 行为和非 search 工具结果不变。直接 `ScopedSearchService` 的公共请求/响应类型 SHALL 保持兼容。

#### Scenario: 真实 pending 继续使用原控制面

- **WHEN** deferred Focus 作业仍在运行
- **THEN** legacy 客户端继续收到现有 `query_id` retry ticket，task-capable 客户端继续由现有 Tasks 转换处理

#### Scenario: 工具目录与 schema 不变

- **WHEN** 比较本变更前后的 `tools/list`
- **THEN** 工具数量、顺序、名称与 input schema 完全一致
