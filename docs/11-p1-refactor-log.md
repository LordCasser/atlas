# P1 重构日志：追平 CodeGraph 产品化索引能力

> 完成时间: 2026-05-20
> 基于 P0 稳定基线 (ReferenceId kind + references_v2→references + SemanticBinder)

---

## 变更概览

| 模块 | 变更 | 文件 |
|------|------|------|
| **类型** | IndexReport, FailureCategory, ExtractionError | `src/types/structs.rs` |
| **类型导出** | 新增类型导出 | `src/types/mod.rs` |
| **提取** | ParseWorkerPool + WorkerConfig | `src/extraction/worker.rs` |
| **提取导出** | 导出 ParseWorkerPool, WorkerConfig | `src/extraction/mod.rs` |
| **搜索** | SearchQueryParser (kind:/lang:/path:/name: 语法) | `src/search/query_parser.rs` |
| **搜索导出** | 导出 query_parser 模块 | `src/search/mod.rs` |
| **同步** | FileLock (SQLite project_metadata based) | `src/sync/file_lock.rs` |
| **同步导出** | 导出 file_lock 模块 | `src/sync/mod.rs` |
| **Store** | acquire_exclusive_lock / release_exclusive_lock | `src/db/store.rs` |
| **测试** | Golden test framework + TS/Python fixtures | `tests/golden.rs`, `tests/fixtures/` |

---

## 新增类型

### FailureCategory

```rust
pub enum FailureCategory {
    ParseTimeout,        // tree-sitter 解析超时
    QueryError,          // tree-sitter 查询异常
    IoError,             // 文件 I/O 错误
    MaxFileSizeExceeded, // 文件超过大小限制
    GrammarPanic,        // Grammar 代码 panic (catch_unwind 隔离)
}
```

### ExtractionError

```rust
pub struct ExtractionError {
    pub file_path: String,
    pub category: FailureCategory,
    pub message: String,
}
```

### IndexReport

```rust
pub struct IndexReport {
    pub files_discovered: usize,
    pub files_indexed: usize,
    pub files_skipped: usize,
    pub files_failed: usize,
    pub failures_by_category: HashMap<String, usize>,
    pub references_total: usize,
    pub references_resolved: usize,
    pub resolution_rate: f64,
    pub duration_ms: u64,
}
```

### WorkerConfig

```rust
pub struct WorkerConfig {
    pub max_file_size_bytes: Option<u64>,  // 默认 4 MiB
    pub parse_timeout_secs: u64,           // 默认 30s (预留，当前未强制)
    pub max_workers: usize,                // 0 = Rayon 默认
}
```

---

## 核心组件

### ParseWorkerPool

```
src/extraction/worker.rs

职责：管理 per-file extraction，提供 panic 隔离和错误收集
能力：
  - panic::catch_unwind 隔离 grammar 崩溃
  - max file size 检查
  - 结构化 ExtractionError 收集
  - into_report() 生成 IndexReport

设计决策：
  - 暂不实现 per-file timeout（LanguageAdapter 非 Send，无法跨线程传递）
  - P2 阶段添加 Send + Sync bound 后实现 timeout
  - 线程安全：内部计数器用 Mutex 保护
```

### SearchQueryParser

```
src/search/query_parser.rs

语法：kind:<kind> lang:<lang> path:<path> name:<name> <freetext>
前缀大小写不敏感
kind 支持别名：fn/func → Function, var → Variable, ctor → Constructor
lang 支持别名：ts → TypeScript, py → Python, c++ → Cpp
未知前缀自动降级为 freetext
```

### FileLock

```
src/sync/file_lock.rs

跨进程互斥：使用 SQLite project_metadata 表存储锁状态
- acquire: 写入 exclusive_lock_pid = "pid:timestamp_ms"
- 如果已有锁且进程存活 → bail
- 如果进程已死 → 自动夺取（stale lock steal）
- release: 删除当前 PID 的锁记录

进程存活检测：kill -0 <pid> (Unix), 保守 assume alive (non-Unix)
不引入任何外部依赖（无 libc, 无 flock）
```

### Golden Test Framework

```
tests/golden.rs

每个语言 fixture:
  tests/fixtures/<lang>/simple.<ext>         ← 输入源码
  tests/fixtures/<lang>/simple.expected.json ← 期望的 extraction 输出

运行方式：
  cargo test --test golden --features typescript,python

Bootstrap 模式：
  如果 expected.json 不存在，自动生成 baseline
  如果存在，比较实际输出与期望，不匹配时报 diff

已覆盖语言：TypeScript, Python
```

---

## 约束遵守

- [x] 不改变 extraction pipeline 的语义逻辑
- [x] 不新增 DB 表
- [x] 不改变 Resolver 行为
- [x] Golden test 只验证 extraction 阶段输出
- [x] 不引入 libc 或 flock 依赖
- [x] 不静默吞错（所有失败结构化记录）

---

## 测试结果

```
cargo test --all-features: 205 passed, 1 ignored
```

新增测试：IndexReport (3) + ParseWorkerPool (4) + SearchQueryParser (12) + FileLock (3) + Golden (2) = 24
