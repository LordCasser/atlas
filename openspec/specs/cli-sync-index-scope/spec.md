# cli-sync-index-scope Specification

## Purpose
定义 `atlas sync` 继承最后一次成功 Index 的持久 include/exclude 范围、校验 scope authority，并使增量 facts 与该范围持续收敛的契约。

## Requirements

### Requirement: sync 必须继承持久 Index scope

系统 MUST 使用最后一次成功 Index 持久化的 `indexed_scope.include` 与 `indexed_scope.exclude` 构造 sync source discovery。普通 hash change detection 与 capability-aware change detection SHALL 复用同一个 scope loader，不得在 CLI 或 detector 分支中默认为不同范围。

#### Scenario: include scope 外文件不可见

- **GIVEN** 持久 scope 的 include 为 `src/**`，且工作区同时存在 `src/main.ts` 与 `other/dep.ts`
- **WHEN** 用户执行 `atlas sync`
- **THEN** discovery 与 dirty-set 只把 `src/**` 内文件作为可新增或重抽取来源，`other/dep.ts` 不得进入 added/modified workset

#### Scenario: exclude 文件不得复活

- **GIVEN** 最后一次成功 Index 以 exclude pattern 排除了一个 canonical discovery 可见文件
- **WHEN** 用户执行 `atlas sync`
- **THEN** 该文件仍被排除，不得因 sync 使用默认 discovery 而重新写入 Store

### Requirement: scope metadata 必须显式校验并安全降级

系统 MUST 将当前有效 scope 解释为同时含 string-array `include` 与 string-array `exclude` 的 JSON object。metadata key 缺失时系统 SHALL 保留 legacy whole-project sync；metadata key 存在但形状或字段类型损坏时系统 MUST fail closed，不得回退到全项目 discovery。

#### Scenario: legacy 数据库没有 scope key

- **GIVEN** 一个可读取的旧数据库没有 `indexed_scope` key
- **WHEN** sync 加载 discovery config
- **THEN** 系统使用空 include/exclude 的 whole-project config，并记录诊断 warning

#### Scenario: 显式 whole-project scope

- **GIVEN** `indexed_scope` 为 `{ "include": [], "exclude": [] }`
- **WHEN** sync 加载 discovery config
- **THEN** 系统按 whole-project discovery 运行

#### Scenario: 损坏 metadata 不得扩大范围

- **GIVEN** `indexed_scope` 是非法 JSON、非 object、缺少必要字段，或 include/exclude 含非字符串元素
- **WHEN** 用户执行 sync
- **THEN** sync MUST 返回错误，不运行 cleanup/extraction，且不得更新 `last_sync_time` 或 `indexed_pipeline_grade`

### Requirement: scoped dirty-set 必须保持完整增量语义

系统 MUST 在持久 scope 内继续支持内容新增、修改、删除和 hash-clean capability upgrade。Store 中存在但不属于 scoped discovered inventory 的缓存路径 SHALL 进入既有 deleted cleanup，以使持久事实集合重新满足 scope 声明；系统不得删除对应工作区源码。

#### Scenario: scope 内 add modify delete

- **WHEN** scope 内分别出现新文件、内容修改和源码删除
- **THEN** sync 将其分类为 added、modified 与 deleted，并通过既有增量阶段收敛 Store

#### Scenario: scoped capability upgrade

- **GIVEN** scope 内文件 hash 未变化但缺目标 capability，scope 外文件也存在于工作区
- **WHEN** 用户以更高 `--analysis` 等级执行 sync
- **THEN** 只有 scope 内缺 capability 的文件进入 reindex workset，并完成目标 extraction/resolution/summary 阶段

#### Scenario: 修复历史 scope 外缓存污染

- **GIVEN** Store 因旧 sync 行为含有 `indexed_scope` 外的 source facts
- **WHEN** 下一次 sync 使用合法持久 scope
- **THEN** scope 外 Store 路径按 files-removed 清理，工作区源文件保持不变，最终 DB inventory 不再越过 scope

### Requirement: 成功 sync 必须保持 scope 与事实终态一致

系统 MUST 在成功 sync 后保留原始合法 `indexed_scope`，并使 files、extraction state、graph/summary facts、目标 pipeline grade 与该 scope 一致。scope 解析或任一必要阶段失败时不得发布新的成功 metadata。

#### Scenario: sync 不重写 scope

- **GIVEN** 一个合法非空 include/exclude scope
- **WHEN** scoped sync 成功完成
- **THEN** `indexed_scope` 值保持不变，现有原子提交只更新目标 grade 与 `last_sync_time`

#### Scenario: scoped 同等级 no-op

- **GIVEN** scope 内 inventory、hash 与目标 capability 均已满足，scope 外文件从未进入 Store
- **WHEN** 用户以相同分析等级再次 sync
- **THEN** 系统不重抽取文件、不改变持久事实，也不刷新 `last_sync_time`

### Requirement: CLI 回归必须验证真实范围边界

系统 SHALL 通过真实 `atlas` binary 与磁盘 SQLite 验证 scoped index 后的跨进程 sync。测试 MUST 以退出状态和重开数据库后的 inventory、scope metadata 与 capability facts 为 oracle，不得依赖完整 stdout/stderr、spinner 或 phase duration。

#### Scenario: scope 外变化跨进程保持隔离

- **WHEN** 回归先执行 scoped index，再在 scope 内外制造新增/修改/删除并执行独立 sync 进程
- **THEN** 重开的 DB 只反映 scope 内变化，scope 外文件不被加入，持久 `indexed_scope` 保持原值

#### Scenario: include exclude 与 capability 组合

- **WHEN** 真实进程从 scoped manifest index 升级至 structural/full，且工作区存在 excluded 或 include 外文件
- **THEN** 只有有效 scope 内文件获得目标 capability，范围外文件在 files、extraction state、graph 与 summary 中均不可见
