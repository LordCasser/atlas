# CancellationToken Architecture — Phase 2-3 Implementation

> **临时设计文档**: CancelCheck trait + checkpoint insertion + coordinator integration.

---

## 1. 目标

在 lazy structural extraction 中引入可中断提取，使 LazyBudget 的超时约束从"循环守卫"升级为"硬中断"。

**核心原则**:
- 不修改 tree-sitter C FFI（parse 完成后检查，不尝试中断 parse）
- 不引入 signal、不引入额外线程
- Cancellation 是正常降级路径，不是错误——必须产生 `budget_exceeded=true` + precision 降级，不能导致 MCP 工具报错
- `extract_file_with_mode()` 的原签名保持不变，新增 `_cancellable` 变体

---

## 2. Crate Boundary: `CancelCheck` trait in extraction crate

### 位置
`crates/atlas-engine/crates/extraction/src/cancel.rs` (新文件)

### API

```rust
/// Trait for cancellation-aware extraction.
///
/// Implementations check whether the current extraction should be
/// cancelled (budget exhausted, explicit user cancellation, etc.).
///
/// This trait lives in the `extraction` crate because it is consumed
/// by `extract_file_with_mode_cancellable` — extraction cannot depend
/// on `atlas-engine`.
pub trait CancelCheck {
    /// Whether the current operation has been cancelled.
    fn is_cancelled(&self) -> bool;
}

/// A CancelCheck that never cancels — used by the original
/// `extract_file_with_mode` wrapper for backward compatibility.
pub(crate) struct NeverCancel;

impl CancelCheck for NeverCancel {
    fn is_cancelled(&self) -> bool { false }
}
```

### 注册到 lib.rs

`crates/atlas-engine/crates/extraction/src/lib.rs`:
```rust
mod cancel;
pub use cancel::CancelCheck;
```

---

## 3. New Entry Point: `extract_file_with_mode_cancellable`

### 位置
`crates/atlas-engine/crates/extraction/src/extract.rs`

### 新增函数 (插入在 `extract_file_with_mode` 之后):

```rust
/// Like [`extract_file_with_mode`] but cancellation-aware.
///
/// Checks `token.is_cancelled()` at strategic checkpoints and returns
/// `Err` with a Cancelled-kind error if the budget is exhausted.
/// The caller must distinguish cancellation from real extraction failure.
pub fn extract_file_with_mode_cancellable(
    frontend: &dyn LanguageFrontend,
    file_id: FileId,
    path: &Path,
    source: &str,
    content_hash: &str,
    mode: ExtractionMode,
    token: &dyn CancelCheck,
) -> Result<FileFacts> {
    // Same body as extract_file_with_mode, with checkpoints
}

/// Wrap the original function with NeverCancel for backward compat.
pub fn extract_file_with_mode(
    frontend: &dyn LanguageFrontend,
    file_id: FileId,
    path: &Path,
    source: &str,
    content_hash: &str,
    mode: ExtractionMode,
) -> Result<FileFacts> {
    extract_file_with_mode_cancellable(
        frontend, file_id, path, source, content_hash, mode, &NeverCancel,
    )
}
```

### 公开导出

`crates/atlas-engine/src/lib.rs` 或 `crates/atlas-engine/crates/extraction/src/lib.rs`:
```rust
pub use extract::{extract_file_with_mode, extract_file_with_mode_cancellable};
```

---

## 4. Checkpoint Insertion Points

### 4a. `extract.rs` — in `extract_file_with_mode_cancellable`

| 检查点 | 位置 (相对原函数流程) | 检查内容 |
|--------|----------------------|----------|
| CP1 | 在 `tl_parse()` 调用之前 | `token.is_cancelled()` — 如果进入函数时预算已耗尽，跳过 parse |
| CP2 | 在符号查询 (symbols `collect_captures`) 之后 | `token.is_cancelled()` — parse 完成后检查 |
| CP3 | 在引用查询 (references `collect_captures`) 之后 | `token.is_cancelled()` — 两个查询后检查 |
| CP4 | 在导入查询 (imports) 之后，作用域查询 (scopes) 之后 | `token.is_cancelled()` |

**错误返回**: `anyhow::bail!("cancelled")` 或自定义错误类型。调用者通过检查 error message 中的 "cancelled" 关键字或使用 `downcast` 判断。

### 4b. `query_helpers.rs` — in `collect_captures`

在 `while let Some((m, i)) = captures.next()` 循环中，每 ~100 次迭代检查一次:

```rust
pub fn collect_captures<'a>(
    query: &tree_sitter::Query,
    root: tree_sitter::Node<'a>,
    source: &'a [u8],
    token: Option<&dyn CancelCheck>,    // NEW parameter
) -> Vec<(tree_sitter::QueryMatch<'a, 'a>, usize)> {
    let mut captures = Vec::new();
    let mut cursor = tree_sitter::QueryCursor::new();
    let mut count = 0usize;
    for (m, i) in cursor.captures(query, root, source) {
        captures.push((m, i));
        count += 1;
        if count % 100 == 0 {
            if let Some(t) = token {
                if t.is_cancelled() {
                    break;
                }
            }
        }
    }
    captures
}
```

**注意**: 不改变返回值类型。Cancelled 时返回部分 captures — 调用者会看到不完整的结果，但后续的阶段检查点 (CP2-CP4) 会捕获并阻止 DB 写入。

### 4c. `lazy_structural.rs` — `reindex_file_structural`

| 检查点 | 位置 (行号) | 检查内容 |
|--------|------------|----------|
| CP5 | 在 `extract_file_with_mode_cancellable()` 调用之前 (~503) | `token.is_cancelled()` |
| CP6 | 在 `replace_file_facts_with_invalidation()` 调用之前 (~527) | `token.is_cancelled()` — 最关键，阻止已完成的解析写入 DB |
| CP7 | 在 `ensure_structural_for_files` 的 `incremental_resolve_and_build` 之前 (~441) | `token.is_cancelled()` |

---

## 5. ReindexOutcome — Cancellation vs. Success

### 位置
`crates/atlas-engine/src/lazy_structural.rs`

### 新枚举

```rust
/// Outcome of a single-file structural reindex.
pub(crate) enum ReindexOutcome {
    /// Extraction and DB write completed successfully.
    Built,
    /// Extraction was cancelled (budget exhausted, token was signalled).
    /// No DB write occurred — the file's extraction state is unchanged.
    Cancelled,
}
```

### `reindex_file_structural` 返回类型变更

从 `Result<()>` 变为 `Result<ReindexOutcome>`:
- `Ok(ReindexOutcome::Built)` — 正常完成
- `Ok(ReindexOutcome::Cancelled)` — 提取被取消，无 DB 写入
- `Err(e)` — 真正的提取失败（与取消无关）

### `ensure_structural_for_files` 处理

```rust
match self.reindex_file_structural(file_id, token) {
    Ok(ReindexOutcome::Built) => {
        result.files_built += 1;
        result.built_file_ids.push(*file_id);
    }
    Ok(ReindexOutcome::Cancelled) => {
        result.budget_exceeded = true;
        break;  // 不再处理后续文件
    }
    Err(e) => {
        tracing::warn!("Lazy structural failed for {:?}: {:#}", file_id, e);
    }
}
```

**关键**: `Cancelled` 设置 `budget_exceeded = true` 但不计入 `files_built`，保证 precision 降级但不谎报构建数量。

---

## 6. `impl CancelCheck for LazyBudget`

### 位置
`crates/atlas-engine/src/lazy_budget.rs`

### 修改

1. 在 `LazyBudget` 中添加字段:
```rust
cancelled: std::sync::atomic::AtomicBool,
```
初始化为 `AtomicBool::new(false)`。

2. 新增方法:
```rust
/// Signal cancellation (called from budget check or externally).
pub(crate) fn cancel(&self) {
    self.cancelled.store(true, Ordering::Release);
}
```

3. 实现 `CancelCheck` trait:
```rust
impl extraction::CancelCheck for LazyBudget {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire) || self.time_exceeded()
    }
}
```

4. 在 `can_continue()` 中集成:
```rust
pub fn can_continue(&self) -> bool {
    if self.time_exceeded() {
        self.cancel();  // 超时时自动触发取消信号
        return false;
    }
    !self.files_exhausted() && !self.cancelled.load(Ordering::Acquire)
}
```

**注意**: `is_cancelled()` 同时检查 `cancelled` 标志 **和** `time_exceeded()`。即使外部未显式调用 `cancel()`，超时也触发取消。

---

## 7. Coordinator Integration

### `lazy_coordinator.rs`

`ensure_structural_with_closure` 调用 `ensure_structural_for_file` 时，传递 `budget` 作为 `&dyn CancelCheck`。

`LazyBudget` 已经实现 `CancelCheck`，且已经通过 `&mut LazyBudget` 传入 coordinator。只需要在调用 `service.ensure_structural_for_file()` 时额外传递作为 token。

修改方案: `LazyStructuralService::ensure_structural_for_file` 需要接受 `Option<&dyn CancelCheck>` 参数。

或者在 coordinator 中直接调用底层的 `ensure_structural_for_files` 并传递 token。

**简化方案**: 在 `lazy_coordinator.rs` 的 `ClaimResult::Claimed` 分支中，替换:
```rust
let build_result = if is_seed {
    service.ensure_structural_for_file(file_id)
} else {
    service.ensure_resolution_symbols_for_file(file_id)
};
```
为:
```rust
let build_result = if is_seed {
    service.ensure_structural_for_file_with_token(file_id, budget as &dyn CancelCheck)
} else {
    service.ensure_resolution_symbols_for_file(file_id)
};
```

`ensure_structural_for_file_with_token` 是新增方法，内部调用 `reindex_file_structural_with_token`，传递 token 到 `extract_file_with_mode_cancellable`。

---

## 8. Background Prewarm — 另行处理

背景 prewarm (`spawn_background_prewarm`) 中的 lazy extraction 不受前台 MCP 工具超时约束。**不需要**传递 CancellationToken — prewarm 线程可以跑完。

---

## 9. Implementation Plan

### Phase A: extraction crate (Agent 1)
1. 创建 `cancel.rs`: `CancelCheck` trait + `NeverCancel`
2. 注册到 `extraction/src/lib.rs`
3. 新增 `extract_file_with_mode_cancellable` (extract.rs)
4. 重构 `extract_file_with_mode` 为 `_cancellable` + `NeverCancel` 包装
5. 插入 CP1-CP4 检查点
6. 修改 `collect_captures` 接受 `Option<&dyn CancelCheck>`
7. `cargo check -p extraction` 验证

### Phase B: engine layer (Agent 2)
8. `LazyBudget` 添加 `cancelled: AtomicBool` + `impl CancelCheck`
9. `lazy_structural.rs`: `ReindexOutcome` 枚举 + `reindex_file_structural` 返回签名变更
10. `lazy_structural.rs`: CP5-CP7 检查点
11. `lazy_coordinator.rs`: coordinator 处理 `ReindexOutcome::Cancelled`
12. `cargo check -p atlas-engine` 验证

### Phase C: 全量验证 (Agent 3)
13. `cargo check --workspace` 编译
14. `cargo test --workspace` 测试
15. 确认原`extract_file_with_mode` 仍然可用（向后兼容）
