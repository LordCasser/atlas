# Atlas 性能优化终极方案（v2 — 含实测数据）

## 前言：方案演进

本方案经过 4 轮对抗性自我否定与约束突破探索 + 多项目实测验证，从最初"77% 提升"的幻想收敛到现实可达的 **全量索引 30-50%、增量同步 20-40% 端到端提升**。

### 实测数据（2026-06-10，Apple Silicon M 系列，release build）

| 项目 | 文件数 | 符号数 | 边数 | 总耗时 | Resolution 占比(估) | 备注 |
|------|--------|--------|------|--------|---------------------|------|
| TypeScript (monorepo) | 1,931 | ~35K | ~65K | **75s** | ~60s (80%) | 244% CPU，大量 rayon 并行 |
| C (FFmpeg-like) | 732 | ~11K | ~51K | **25.5s** | ~18s (70%) | — |
| Rust (Atlas 自身) | 104 | ~3.5K | ~15K | **53.7s** | ~45s (84%) | 重新索引 stdlib，文件少但解析慢 |
| Java | 152 | ~5.6K | — | **3.9s** | ~2.5s (64%) | Import 解析 ~500ms |
| Atlas 自身 (旧) | 156 | ~8K | — | 28.1s | 22.3s (79%) | docs 中的历史数据 |

### 实测关键发现

1. **Resolution 热路径 ZERO instrumentation**：`fuzzy_search`、`resolve_one_core`、策略分发均无 tracing span。无法从日志判断 45-60s 的 Resolution 时间花在哪——是 Levenshtein CPU、S4 线性扫描、S6 缓存竞争、还是 ImportResolver 的 DB 查询。

2. **QName DB 查询每次 2-3ms，无缓存**：Java 项目中 `store.find_symbols_by_qname` 每次调用 2-3ms，同一模块重复查询时间不减。1,100 次查询合计 ~550ms。高引用项目中这个开销会放大。

3. **串行路径批量写入已验证效果**：commit d6c9517b 将串行解析路径从 DB 写入 283s 优化到 5.9s（**48x**），核心是 500/2000 批量写入。批量优化的效果已有实证。

4. **Rust 项目异常慢**：104 文件 53.7s 远超预期。推测是 stdlib 重新索引（每次提取所有 std 符号）或某个特定语言 adapter 的解析极慢。

5. **主流痛点不在 Extraction**：extraction 在所有项目中占比 <10%。重点在 Resolution + DB Write。

关键认识：
1. **Resolution 瓶颈主因尚未确定**——缺 profiling 数据。可能是 Levenshtein CPU、S4 线性扫描、ImportResolver QName 查询、或 S6 的 Mutex 竞争。
2. **QName 缓存是最低成本的确定性优化**——实测已证每次 2-3ms 无缓存。
3. **组合方案远超单一优化**。文件本地预解析 + QName 缓存 + 批量写入的叠加效果是乘法。

---

## Part A: Phase 0 — 紧急前置动作（1 天，最高优先级）

### 现状：Resolution 热路径是一片黑盒

**这是当前最大的问题。** 我们没有数据判断 45-60s 的 Resolution 时间具体花在哪：
- Levenshtein CPU 计算？（需要精确时间）
- S4 线性扫描？（需要 S4 命中次数和耗时）
- S6 大量 miss → fuzzy_search？（需要 fuzzy_search 调用次数、缓存命中率、候选集大小）
- ImportResolver QName DB 查询？（需要 S5 调用次数和 QName 查询耗时分布）
- Step A 串行构建 ResolutionContext？（需要 Step A/B 时间比）
- GlobalSymbolIndex 构建？（需要构建耗时）

必须先让 resolution 热路径**可观测**，再决定花时间优化哪个环节。

### A1. 添加 resolution 热路径 tracing（必须最先做）

在以下关键路径插入 `tracing::span!(Level::INFO, ...)` + 计数器：

在以下关键路径插入 `tracing::span!(Level::INFO, "name")`：

| Span | 测量目标 | 位置 |
|------|---------|------|
| `resolution.step_a` | Step A 构建 ResolutionContext 耗时 | `lib.rs:484-492` |
| `resolution.step_b` | Step B 并行解析耗时 | `lib.rs:510-530` |
| `resolution.phase2` | Phase 2 写回 resolves 耗时 | `lib.rs:550-570` |
| `resolution.strategy.{s1..s6}` | 各策略命中次数与耗时 | `resolve_one_core` 中 |
| `resolution.fuzzy_search` | fuzzy_search 调用次数、缓存命中率、候选集大小 | `context.rs:156` |
| `resolution.import_resolve` | Import 解析中 DB 查询次数 | `import_resolver.rs:78` |
| `db.write_symbols` | 符号写入耗时 | `store_writers.rs:22` |
| `db.write_scopes` | 作用域写入耗时 | `store_writers.rs` write_scopes |
| `db.write_references` | 引用写入耗时 | `store_writers.rs:194` |
| `db.write_data_nodes` | 数据节点写入耗时 | `store_writers.rs` write_data_nodes |
| `db.write_dataflow_edges` | 数据流边写入耗时 | `store_writers.rs` write_dataflow_edges |
| `db.fk_guard` | FK 守卫过滤耗时 | `store_writers.rs:584-711` |
| `extract.query_compile` | Query::new 编译耗时 | `extract.rs` extract_and_normalize |
| `extract.file_local_resolve` | 文件本地解析耗时（新增） | 见 Part B T2-1 |
| `graph.build_all` | GraphBuilder 总耗时 | `graph_builder.rs` |
| `graph.symbol_cache` | Symbol cache 预加载耗时 | `graph_builder.rs:50-56` |
| `sync.incremental.cleanup` | 增量同步清理耗时 | `incremental_pipeline.rs` |
| `sync.full.cleanup` | 全量同步清理耗时 | `index_pipeline_orchestrator.rs` |

**产出**：一份 profiling 报告，用于调整 T2/T3 优化项的优先级。

### A2. 建立性能基准测试

添加 `crates/atlas-engine/benches/` 下的 benchmark：

```rust
// benches/resolution_bench.rs
// 对固定测试项目（Atlas 自身 + TS test project）测量：
// - resolve_all_parallel 总耗时
// - 各策略命中分布
// - fuzzy_search 缓存命中率

// benches/db_write_bench.rs
// 对固定数据集测量：
// - insert_file_facts_batch 耗时
// - 各 write_* 函数耗时

// benches/end_to_end_bench.rs  
// 完整 index pipeline 耗时
```

**产出**：CI 中的性能回归检测，每次 PR 自动运行。

**状态：Phase 0 completed（2026-06-10）**。Resolution 和 DB tracing spans 已添加。正确的 profiling 过滤方式：使用 `--log-format json` 配合显式目标过滤如 `atlas_resolve=debug,atlas_db=debug,atlas_extract=debug`。

---

## Part B: 优化方案（按执行顺序）

### B-T0: 零争议优化（立即做，确定性收益）

所有 T0 优化**不打破任何约束**，可独立验证。原计划总计 <200 行代码改动（实际实现中 T0.4 经代码调查确认 O(N²) 不存在，已移除）。每个优化都有实测数据支撑或理论保障。

#### B-T0.1: ImportResolver QName 查询缓存 ⭐ 新增（实测驱动）

**实测依据**：Java 项目中 `store.find_symbols_by_qname` 每次调用 2-3ms，同一模块 `brut.xml.XmlUtils` 连续查询 2 次都是 2.9ms + 2.3ms，无任何缓存。1,100 次查询合计 ~550ms。在 TS 项目中（1,931 文件），import 更多，QName 查询开销可放大到数秒。

**文件**: `crates/atlas-engine/crates/resolution/src/import_resolver.rs`

```rust
pub struct ImportResolver {
    path_alias_resolver: Option<PathAliasResolver>,
    project_root: PathBuf,
    /// NEW: QName → SymbolId cache (cleared per session)
    qname_cache: RefCell<HashMap<String, Option<SymbolId>>>,
}

impl ImportResolver {
    pub fn resolve_import(
        &self,
        store: &Store,
        import: &ImportDef,
        // ...
    ) -> anyhow::Result<Vec<ResolvedTarget>> {
        // For each candidate QName:
        for qname in candidate_qnames {
            // Try cache first
            if let Some(cached) = self.qname_cache.borrow().get(&qname) {
                if let Some(id) = cached {
                    results.push(ResolvedTarget { symbol_id: id.clone(), ... });
                    continue;
                } else {
                    continue; // known miss
                }
            }
            
            // Cache miss: query DB (2-3ms)
            let result = store.find_symbols_by_qname(&qname)?;
            self.qname_cache.borrow_mut().insert(
                qname.clone(),
                result.first().map(|s| s.id.clone()),
            );
            // ... use result ...
        }
    }
}
```

**为什么用 `RefCell` 而非 `Mutex`**：`ImportResolver` 在 `resolve_all_parallel` 的 Step B 中被多线程只读共享（`Arc<ImportResolver>`）。`RefCell` 不支持 `Sync`，不能放在 `Arc` 中跨线程访问。

**正确方案**：`qname_cache: Mutex<HashMap<String, Option<SymbolId>>>` 或使用 `dashmap`。

更好方案：将缓存挂在 `ResolutionSession` 上（已经是 per-session 的），使用 `Mutex<HashMap<...>>`。由于 Step B 是并行但只读为主（缓存先检查，未命中时才获取写锁），`Mutex` 竞争不高。

**预期**：Import QName 解析时间减少 80-95%（第一次查询后全命中缓存）。Java 项目 import 解析从 550ms → ~50ms。TS 项目预期减少 2-5s。

#### B-T0.2: Levenshtein Banding（带阈值的早期终止）

**文件**: `crates/atlas-engine/crates/types/src/lib.rs`

当前实现计算完整 O(N×M) 距离，但 fuzzy_search 只需要知道 `dist <= max_distance`（通常为 2）。

新增函数：
```rust
/// Levenshtein distance with early termination at max_dist.
/// Returns None if distance > max_dist, Some(dist) otherwise.
pub fn levenshtein_bounded(a: &str, b: &str, max_dist: usize) -> Option<usize> {
    let a_len = a.chars().count();
    let b_len = b.chars().count();
    
    // Length prune: distance >= abs(len(a) - len(b))
    if a_len.abs_diff(b_len) > max_dist {
        return None;
    }
    
    // Use bytes if both ASCII (95%+ of symbol names)
    if a.is_ascii() && b.is_ascii() {
        return levenshtein_bounded_bytes(a.as_bytes(), b.as_bytes(), max_dist);
    }
    
    // Unicode fallback
    levenshtein_bounded_chars(&a.chars().collect::<Vec<_>>(), &b.chars().collect::<Vec<_>>(), max_dist)
}

fn levenshtein_bounded_bytes(a: &[u8], b: &[u8], max_dist: usize) -> Option<usize> {
    let n = a.len();
    let m = b.len();
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];
    
    for i in 1..=n {
        curr[0] = i;
        let mut row_min = curr[0];
        for j in 1..=m {
            let cost = if a[i-1] == b[j-1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j-1] + 1).min(prev[j-1] + cost);
            row_min = row_min.min(curr[j]);
        }
        // EARLY TERMINATION: entire row > max_dist, cannot recover
        if row_min > max_dist {
            return None;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    
    let dist = prev[m];
    if dist <= max_dist { Some(dist) } else { None }
}
```

**修改 fuzzy_search**（`context.rs:189-217`）：
```rust
// Before: full Levenshtein for every candidate
let dist = levenshtein(&lower_name, candidate_lower);

// After: bounded Levenshtein with early termination
let dist = match levenshtein_bounded(&lower_name, candidate_lower, max_distance) {
    Some(d) => d,
    None => continue, // skip this candidate
};
```

**预期**：模糊搜索计算量减少 60-80%。代码改动 ~40 行。

> **注意**：`fuzzy_search` 已内置 trigram 预过滤 + 结果缓存。Banding 优化作用于通过 trigram 过滤的候选集，因此其绝对收益低于在朴素线性扫描上的效果。实际收益需要 profiling 数据量化。

#### B-T0.3: ResolutionContext by_name 预索引

**文件**: `crates/atlas-engine/crates/resolution/src/context.rs`

```rust
pub struct ResolutionContext {
    // ... existing fields
    
    /// NEW: O(1) lookup by name within this file
    symbols_by_name: HashMap<String, Vec<Arc<SymbolDef>>>,
}

impl ResolutionContext {
    pub fn build(store: &Store, file_id: &FileId) -> anyhow::Result<Self> {
        // ... existing code ...
        
        let mut symbols_by_name: HashMap<String, Vec<Arc<SymbolDef>>> = HashMap::new();
        for sym in &symbols {
            symbols_by_name
                .entry(sym.name.to_lowercase())
                .or_default()
                .push(Arc::clone(sym));
        }
        
        Ok(Self { symbols_by_name, /* ... */ })
    }
    
    pub fn find_in_file_by_name(&self, name: &str) -> Vec<Arc<SymbolDef>> {
        self.symbols_by_name
            .get(&name.to_lowercase())
            .cloned()
            .unwrap_or_default()
    }
}
```

**预期**：Strategy 4（同文件精确查找）从 O(F) 降为 O(1)。对大型文件影响显著。

#### B-T0.5: replace_dataflow_for_unit 批量 DELETE ✅ 已实现

**文件**: `crates/atlas-engine/crates/db/src/store/unit_extraction_state.rs`

```rust
// Before: N+1 pattern
for dn_id in &dn_ids {
    tx.execute("DELETE FROM dataflow_edges WHERE source = ?1 OR target = ?1", params![dn_id])?;
}

// After: single statement
tx.execute(
    "DELETE FROM dataflow_edges WHERE source IN (SELECT data_node_id FROM data_nodes WHERE function_id = ?1)
        OR target IN (SELECT data_node_id FROM data_nodes WHERE function_id = ?1)",
    params![function_id],
)?;
```

**预期**：Lazy dataflow 重建时删除操作从 O(N) DB 调用降为 1 次。

---

### B-T1: 高置信度优化（Profiling 确认后执行）

所有 T1 优化**不打破任何约束**，但代码改动较大，需要更多测试。

#### B-T1.1: 批量 INSERT 扩展

**文件**: `crates/atlas-engine/crates/db/src/store_writers.rs`

**通用批量 INSERT 构造器**：

```rust
/// Dynamically-sized batch chunk based on params_per_row
fn batch_chunk_size(params_per_row: usize) -> usize {
    // SQLITE_MAX_VARIABLE_NUMBER default = 999
    // Leave margin for safety
    900 / params_per_row
}

fn write_batched<T>(
    tx: &Transaction,
    table: &str,
    columns: &[&str],
    items: &[T],
    params_per_row: usize,
    bind_fn: impl Fn(&T, &mut Vec<Box<dyn rusqlite::types::ToSql>>),
) -> anyhow::Result<()> {
    if items.is_empty() { return Ok(()); }
    
    let chunk_size = batch_chunk_size(params_per_row);
    for chunk in items.chunks(chunk_size) {
        let placeholders: Vec<String> = (0..chunk.len())
            .map(|i| {
                let start = i * params_per_row + 1;
                let params: Vec<String> = (start..start + params_per_row)
                    .map(|p| format!("?{}", p))
                    .collect();
                format!("({})", params.join(", "))
            })
            .collect();
        
        let sql = format!(
            "INSERT OR REPLACE INTO {} ({}) VALUES {}",
            table,
            columns.join(", "),
            placeholders.join(", ")
        );
        
        let mut stmt = tx.prepare(&sql)?;
        let mut params: Vec<Box<dyn ToSql>> = Vec::with_capacity(chunk.len() * params_per_row);
        for item in chunk {
            bind_fn(item, &mut params);
        }
        let param_refs: Vec<&dyn ToSql> = params.iter().map(|p| p.as_ref()).collect();
        stmt.execute(param_refs.as_slice())?;
    }
    Ok(())
}
```

**受影响的实体**（优先级按预估行数排序）：

| # | 实体 | 每行参数 | Chunk Size | 预估行数(F) |
|---|------|---------|------------|------------|
| 1 | dataflow_edges | 9 | 100 | 5K-50K |
| 2 | data_nodes | 32 | 28 | 5K-50K |
| 3 | cfg_edges | 6 | 150 | 1K-20K |
| 4 | cfg_nodes | 7 | 128 | 1K-20K |
| 5 | binding_uses | 4 | 225 | 1K-20K |
| 6 | edges | 8 | 112 | 5K-50K |
| 7 | scopes | 7 | 128 | 1K-10K |
| 8 | callsites | 16 | 56 | 500-5K |
| 9 | imports | 17 | 52 | 100-2K |
| 10 | bindings | 4 | 225 | 1K-10K |

**实施策略**：先做 dataflow_edges + data_nodes + edges（行数最多的三个），验证效果后再做其余。

**预期**：按实体类型分组写入（不交错），利用 SQLite page cache 局部性。DB Write 时间减少 15-30%。

> **状态**：T1.1 的 `dataflow_edges` + `edges` 批量 INSERT 已实现。

#### B-T1.2: Cleanup 事务包裹

**文件**: `crates/atlas-engine/crates/filesync/src/cleanup.rs`

```rust
pub fn clean_stale_file_ids(store: &Store, file_ids: &[FileId]) -> anyhow::Result<()> {
    if file_ids.is_empty() { return Ok(()); }
    store.with_transaction(|tx| {
        for file_id in file_ids {
            store.invalidate_references_to_symbols_in_file_in_tx(tx, file_id)?;
            store.delete_edges_for_file_references_in_tx(tx, file_id)?;
        }
        store.delete_files_batch_in_tx(tx, file_ids)?;
        Ok(())
    })
}
```

需要新增一批 `_in_tx` 变体函数，或使用闭包传递 transaction。

**预期**：大量 dirty 文件的增量同步清理时间减少 50-80%。

#### B-T1.3: Tree-sitter Query 缓存

**文件**: `crates/atlas-engine/crates/extraction/src/frontend.rs`

```rust
use std::sync::OnceLock;

pub struct LanguageFrontend {
    // ... existing fields
    def_query: OnceLock<tree_sitter::Query>,
    ref_query: OnceLock<tree_sitter::Query>,
    import_query: OnceLock<tree_sitter::Query>,
    scope_query: OnceLock<tree_sitter::Query>,
    lexical_query: OnceLock<tree_sitter::Query>,
    dataflow_query: OnceLock<tree_sitter::Query>,
}

impl LanguageFrontend {
    pub fn definition_query(&self) -> &tree_sitter::Query {
        self.def_query.get_or_init(|| {
            let lang = self.parser.tree_sitter_language();
            tree_sitter::Query::new(lang, self.symbols.definition_query()).unwrap()
        })
    }
    // ... similar for other queries
}
```

需要确认 `tree_sitter::Query` 是 `Send + Sync`。`OnceLock` 保证线程安全的懒加载。

**预期**：提取阶段 Query 编译时间减少 90%+（但占总提取时间比例需要 profiling 确认，可能在 5-15% 之间）。

#### B-T1.4: GlobalSymbolIndex 索引方案

**文件**: `crates/atlas-engine/crates/resolution/src/context.rs`

```rust
pub struct GlobalSymbolIndex {
    symbols: Vec<SymbolDef>,                    // 唯一所有者（不再 clone）
    lower_names: Vec<String>,                    // 预计算的小写名
    by_name: HashMap<String, Vec<usize>>,        // name → indices（改为 usize）
    by_id: HashMap<SymbolId, usize>,             // id → index（改为 usize）
    file_parent_dir: HashMap<FileId, String>,
    fuzzy_cache: Mutex<HashMap<(String, usize), Vec<usize>>>,  // 缓存索引
    proximity_cache: Mutex<HashMap<(String, FileId), Vec<usize>>>,
}

impl GlobalSymbolIndex {
    pub fn build(store: &Store) -> anyhow::Result<Self> {
        let all_symbols = store.get_all_symbols()?;
        let file_entries = store.list_files()?;
        
        let lower_names: Vec<String> = all_symbols.iter()
            .map(|s| s.name.to_lowercase())
            .collect();
        
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_id: HashMap<SymbolId, usize> = HashMap::new();
        
        for (idx, sym) in all_symbols.iter().enumerate() {
            by_name.entry(lower_names[idx].clone())
                .or_default()
                .push(idx);  // ← 存储索引，不 clone SymbolDef
            by_id.insert(sym.id.clone(), idx);
        }
        
        // ... file_parent_dir ...
        
        Ok(Self {
            symbols: all_symbols, // 只有一个所有者
            lower_names,
            by_name,
            by_id,
            file_parent_dir,
            fuzzy_cache: Mutex::new(HashMap::new()),
            proximity_cache: Mutex::new(HashMap::new()),
        })
    }
    
    /// 新增辅助方法
    pub fn get_by_idx(&self, idx: usize) -> &SymbolDef {
        &self.symbols[idx]
    }
}
```

**访问点修改**（集中在 4 个函数内）：
- `find_by_name`: `by_name.get(name).map(|indices| indices.iter().map(|&i| self.symbols[i].clone()))` 
  - 注意：这里仍然 clone 了，因为调用者需要 SymbolDef。但 clone 次数从 3x 降到 1x（仅调用时克隆需要的）。
- `find_by_id`: `by_id.get(id).map(|&i| self.symbols[i].clone())`
- `fuzzy_search`: 返回 `Vec<usize>`，调用者按需克隆。

**相比 Arc 方案的优势**：
1. 零原子操作开销（Arc::clone/Arc::drop 涉及 atomic inc/dec）
2. 不阻碍未来的分片索引演进
3. 内存更紧凑（usize 8 bytes vs Arc 8 bytes + 引用计数块 16 bytes）

**预期**：GlobalSymbolIndex 构建内存减少 60%，构建时间减少 20-30%。

#### B-T1.5: GraphBuilder Symbol Cache 批量加载

**文件**: `crates/atlas-engine/crates/db/src/store/symbols.rs`（新增 API）
**文件**: `crates/atlas-engine/crates/graph/src/graph_builder.rs`

新增 `Store` API：
```rust
pub fn find_symbols_by_ids(&self, ids: &[SymbolId]) -> anyhow::Result<Vec<SymbolDef>> {
    if ids.is_empty() { return Ok(vec![]); }
    // 分块查询以避免超过 SQLITE_MAX_VARIABLE_NUMBER
    let mut result = Vec::with_capacity(ids.len());
    for chunk in ids.chunks(900) {
        let placeholders = chunk.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
        // 使用 hex 字符串比较（SymbolId 存储为 TEXT/十六进制）
        let sql = format!("SELECT * FROM symbols WHERE hex(symbol_id) IN ({})", placeholders);
        // ... execute and collect ...
    }
    Ok(result)
}
```

**修改 GraphBuilder**：
```rust
// Before: serial loop of individual queries
let mut symbol_cache = HashMap::new();
for (_, target) in resolved {
    if !symbol_cache.contains_key(&target.symbol_id) {
        symbol_cache.insert(target.symbol_id, store.find_symbol_by_id(&target.symbol_id)?);
    }
}

// After: collect IDs, batch query
let ids: Vec<SymbolId> = resolved.iter()
    .map(|(_, t)| t.symbol_id.clone())
    .collect::<HashSet<_>>()
    .into_iter()
    .collect();
let symbols = store.find_symbols_by_ids(&ids)?;
let symbol_cache: HashMap<_, _> = symbols.into_iter().map(|s| (s.id, s)).collect();
```

**预期**：GraphBuilder symbol cache 预加载从 O(N) DB 查询降为 O(1) 批查询。

---

### B-T2: 突破约束的优化（Profiling 门控 + 用户确认）

以下优化打破了现有约束，需要确认 profiling 数据支撑 + 用户同意约束变更。

#### B-T2.1: 文件本地预解析（File-Local Pre-Resolution）

**打破的约束**：`architecture.md` 规定 "Extraction must not do cross-file resolution"

**遵守的方式**：只在文件内部解析（S1-S4），不跨文件。这严格来说没有违反约束——约束禁止的是"跨文件"解析。

**实现**：

```rust
// crates/atlas-engine/crates/extraction/src/extract.rs
// 在 extract_file_with_mode_cancellable 的末尾（步骤 26 之后, 27 之前）

/// Resolve same-file references immediately using symbols in this FileFacts.
/// This eliminates ~20-40% of references from the global resolution pass.
fn resolve_file_local(facts: &mut FileFacts) {
    if facts.references.is_empty() {
        return;
    }
    
    // Build file-local symbol index
    let symbols_by_name: HashMap<&str, Vec<&SymbolDef>> = facts.symbols.iter()
        .fold(HashMap::new(), |mut map, sym| {
            map.entry(sym.name.as_str()).or_default().push(sym);
            map
        });
    
    let mut resolved_count = 0;
    for ref_use in &mut facts.references {
        if ref_use.resolved_symbol_id.is_some() {
            continue; // already resolved (shouldn't happen in extraction, but defensive)
        }
        
        // S1: Builtin filter
        if BuiltinFilter::is_builtin_inline(&ref_use.name, facts.file.language) {
            continue;
        }
        
        // S4: Same-file exact match (scope-local S2/S3 require scope tree, skip for now)
        if let Some(candidates) = symbols_by_name.get(ref_use.name.as_str()) {
            if candidates.len() == 1 {
                ref_use.resolved_symbol_id = Some(candidates[0].id.clone());
                resolved_count += 1;
            }
            // Multiple candidates: defer to global resolution (needs scope info)
        }
    }
    
    tracing::debug!(
        file = %facts.file.path,
        resolved = resolved_count,
        total = facts.references.len(),
        "file-local pre-resolution"
    );
}
```

**关键设计决策**：只做 S1 + S4，不做 S2（需要 scope tree，在提取阶段尚未完全构建）和 S3。这确保了解析的保守性（宁可少解析，不可错解析）。

**预期**：20-40% 的引用在提取阶段被解析（免费，因为数据已在内存）。

**风险**：极低。解析使用的是同一文件的符号（确定性），如果解析错误，后续全局解析也不会覆盖（因为 resolve_one_core 的 first-match-wins 设计）。需要在全局解析中确保已解析的引用不被覆盖。

**补救**：在 `resolve_one_core` 开始处增加检查：
```rust
if reference.resolved_symbol_id.is_some() {
    return None; // already resolved by file-local pass
}
```

#### B-T2.2: 内存全局解析（In-Memory Global Resolution Before DB Write）

**打破的约束**：
1. 管线阶段分离：Resolution 从 Phase 7 移至 Phase 5/6 之间
2. "Resolution does not delete refs" → Refs 一次性写入（含 resolved_symbol_id），无需后续 UPDATE

**不打破的约束**：
- Extraction 不跨文件解析（校验通过）
- DB Schema V1 不变
- 引用解析结果仍然写入 DB

**实现**：在 `IndexPipeline` / `IncrementalPipeline` 中添加新的"阶段合并"路径：

```rust
// crates/atlas-engine/crates/filesync/src/index_pipeline_orchestrator.rs

// NEW: Combined extraction + resolution phase (replaces Phase 5+6+7)
fn phase_extract_resolve_and_write(
    &self,
    dirty_files: &[PathBuf],
    frontends: &HashMap<Language, Arc<LanguageFrontend>>,
    existing_symbols: Option<&[SymbolDef]>,  // for incremental: DB symbols
    existing_imports: Option<&[ImportDef]>,   // for incremental: DB imports
) -> anyhow::Result<IndexPipelineStats> {
    
    // Step 1: Extract all files (parallel, existing code)
    let mut all_facts: Vec<FileFacts> = phase_extract_parallel(dirty_files, frontends)?;
    
    // Step 2: File-local pre-resolution (parallel, B-T2.1)
    all_facts.par_iter_mut().for_each(|facts| {
        resolve_file_local(facts);
    });
    
    // Step 3: Build GlobalSymbolIndex from in-memory facts (+ DB symbols if incremental)
    let mut all_symbols: Vec<&SymbolDef> = all_facts.iter()
        .flat_map(|f| f.symbols.iter())
        .collect();
    if let Some(existing) = existing_symbols {
        all_symbols.extend(existing.iter());
    }
    let global_index = GlobalSymbolIndex::from_symbols(&all_symbols);
    
    // Step 4: Global resolution (parallel)
    let unresolved_refs: Vec<&mut ReferenceUse> = all_facts.iter_mut()
        .flat_map(|f| f.references.iter_mut())
        .filter(|r| r.resolved_symbol_id.is_none())
        .collect();
    
    // Use a similar parallel strategy as current resolve_all_parallel
    resolve_global_parallel(&unresolved_refs, &global_index, &all_facts)?;
    
    // Step 5: Unified DB write (single transaction per batch)
    store.enter_bulk_write()?;
    let _guard = BulkWriteGuard::new(&store);
    
    for chunk in all_facts.chunks(500) {
        store.insert_file_facts_batch(chunk)?;
        if chunk_idx % checkpoint_interval == 0 {
            store.checkpoint_wal()?;
        }
    }
    
    drop(_guard); // restore FK, synchronous
    Ok(stats)
}
```

**新增依赖**：
- `GlobalSymbolIndex::from_symbols(&[&SymbolDef])` — 从引用构建（替代 `build(store)`）
- `resolve_global_parallel` — 并行全局解析（类似当前 `resolve_all_parallel` 的 Step B，但纯内存）
- `ImportResolver` 需要改造以支持内存查询（当前通过 `store.find_symbols_by_qname()`）

**ImportResolver 改造**：
```rust
pub struct ImportResolver {
    // 新增：内存中的全局符号索引（避免 DB 查询）
    global_qname_index: HashMap<String, Vec<SymbolId>>,
}

impl ImportResolver {
    fn try_resolve_global(&self, name: &str) -> Option<SymbolId> {
        // 先在内存索引中查找，找不到再回退 DB
        self.global_qname_index.get(name)
            .and_then(|ids| ids.first())
            .cloned()
    }
}
```

**增量同步的特殊处理**：
对于增量同步，未变更文件中的引用可能指向新提取的符号（这些引用之前在 DB 中 unresolved）。需要：

1. 从 DB 读取所有 unresolved 引用
2. 对于指向"新提取符号"的引用，用新的 global_index 重新解析
3. 写入更新后的引用

这增加了复杂性，但可以通过以下方式简化：
- 对于增量同步，只在新提取的文件中做文件本地预解析
- 全局解析仍然走当前的 resolve_all_parallel 路径
- 只有"全量索引"才使用 in-memory 全局解析

**这个折中方案更实际**：
- 全量索引：使用 in-memory 全局解析（激进路线）
- 增量同步：使用现有 resolve_all_parallel 路径（保守路线）
- 两者共享文件本地预解析（都有收益）

**预期**：
- 全量索引：Resolution 时间减少 30-50%（消除 DB 读写往返）
- 增量同步：Resolution 时间减少 10-20%（仅文件本地预解析收益）

**风险与缓解**：

| 风险 | 缓解 |
|------|------|
| 全量索引内存峰值增大 | 分块处理（500 文件一批），每批独立提取+解析+写入 |
| ImportResolver 需双重实现（DB + 内存） | 先从 DB 批量预加载所有 imports 到内存，统一用内存查询 |
| 全量索引 Resolution 阶段消失导致进度报告不连续 | 保留阶段抽象，将"提取+解析+写入"报告为单个复合阶段 |
| 错误处理变复杂 | 分块处理天然提供错误隔离（一块失败不影响其他） |

#### B-T2.3: write_symbols container_id 预计算

**打破的约束**：无（仅实现优化）。

**文件**: `crates/atlas-engine/crates/db/src/store_writers.rs`

```rust
fn write_symbols(tx: &Transaction, symbols: &[SymbolDef], layer: &str) -> anyhow::Result<()> {
    if symbols.is_empty() { return Ok(()); }
    
    // Pre-compute valid symbol IDs in this batch
    let valid_ids: HashSet<&SymbolId> = symbols.iter().map(|s| &s.id).collect();
    
    let chunk_size = batch_chunk_size(SYM_PARAMS);
    for chunk in symbols.chunks(chunk_size) {
        // ... build INSERT SQL ...
        
        for s in chunk {
            // Compute container_id: use if in valid_ids, otherwise NULL
            let container_id = s.container.as_ref()
                .filter(|cid| valid_ids.contains(cid));
            
            all_params.push(Box::new(s.id.as_ref()));
            // ... other params ...
            all_params.push(Box::new(container_id)); // ← pre-computed
        }
    }
    
    // NO SECOND PASS NEEDED — container_id is already set
}
```

**注意**：此优化依赖 `foreign_keys=OFF`（在 bulk_write 模式下）。如果 FK=ON，跨文件 container 引用的符号可能尚未写入 → INSERT 失败。

**安全策略**：仅在 bulk_write 模式（FK=OFF）下使用直接填充。正常模式（FK=ON）保留两步设计。

**预期**：Symbol 写入时间减少 20-30%（消除二次 UPDATE 循环）。

---

### B-T3: 门控优化（Profiling 确认后才做）

以下优化需要 B-T0.5（tracing）的 profiling 数据来确认收益，可能不需要做。

#### B-T3.1: BK-Tree / Trie 模糊搜索索引

**门控条件**：`fuzzy_search` miss_rate > 30% AND 候选集平均大小 > 100。

**实现选项 A — Trie + Edit Distance**：
构建 Trie，模糊搜索时用编辑距离剪枝遍历。对 max_distance=2，访问节点数 O(26² × L) ≈ O(10K)（远小于线性扫描的 O(100K)）。

**实现选项 B — 直接优化候选集大小**：
在 fuzzy_search 中增加方向性过滤：
```rust
// 按文件目录近邻过滤候选
fn fuzzy_search_proximity(&self, name: &str, max_dist: usize, near_file: &FileId) -> Vec<usize> {
    let parent_dir = &self.file_parent_dir[near_file];
    let candidates = self.lower_names.iter()
        .enumerate()
        .filter(|(_, n)| n.len().abs_diff(name.len()) <= max_dist)
        .filter(|(idx, n)| {
            // Only search symbols in same directory or parent
            let sym_file_id = &self.symbols[*idx].file_id;
            let sym_dir = &self.file_parent_dir[sym_file_id];
            sym_dir == parent_dir || parent_dir.starts_with(sym_dir)
        })
        .collect::<Vec<_>>();
    // ... Levenshtein on filtered candidates ...
}
```

**预期**：候选集从 100K → 1K，fuzzy_search 时间减少 90%。

**风险**：跨目录引用可能被遗漏。作为 fallback：先 proximity 搜索，如果没找到，再全局搜索。

#### B-T3.2: 分块提取+写入（Chunked Extract-Write）

**门控条件**：全量索引内存峰值 > 1GB 或 DB Write 有显著的 idle CPU 时间。

**实现**：将当前 "提取全部 → 写入全部" 改为循环：
```rust
for chunk in dirty_files.chunks(500) {
    let extracted = phase_extract_parallel(chunk)?;
    phase_write_batched(&extracted)?;
    // chunk 在此释放内存
}
```

**注意**：如果结合 B-T2.2（in-memory 全局解析），分块需要在每个 chunk 内完成局部解析，但不能做完整的全局解析（因为其他 chunk 的符号还未提取）。全局解析仍需在最后对所有 unresolved refs 执行。

**折中**：
```rust
// 阶段 A：分块提取 + 文件本地解析 + 分批写入
for chunk in dirty_files.chunks(500) {
    let extracted = phase_extract_parallel(chunk)?;
    for facts in &mut extracted {
        resolve_file_local(facts);  // B-T2.1
    }
    phase_write_batched(&extracted)?;
}

// 阶段 B：从 DB 加载所有符号，对所有 unresolved refs 做全局解析
let all_symbols = store.get_all_symbols()?;
let global_index = GlobalSymbolIndex::from_symbols_iter(all_symbols);
let unresolved_refs = store.get_unresolved_refs()?;
resolve_global_parallel(&unresolved_refs, &global_index)?;
store.batch_update_resolved_refs(&unresolved_refs)?;
```

这回到了当前的 DB 往返，但至少文件本地预解析仍然有效。
**由于 B-T2.2 的激进路线更优，此方案作为 fallback**。

---

## Part C: 基于实测数据的综合效果预估

### 保守估计（T0 确定收益 + T1 高概率）

| 项目 | 当前 | T0 后 | T0+T1 后 | 改进 |
|------|------|-------|---------|------|
| TS monorepo (1,931 文件) | 75s | 55-60s | 45-50s | -33% to -40% |
| C (732 文件) | 25.5s | 20-22s | 17-19s | -25% to -33% |
| Java (152 文件) | 3.9s | 3.0-3.3s | 2.7-3.0s | -23% to -31% |
| Rust/Atlas (104 文件) | 53.7s | 37-42s | 30-35s | -30% to -44% |

**T0 收益分解**（基于实测数据推算）：

| T0 优化 | TS 收益(估) | Java 收益(测) | 确定性 |
|---------|------------|-------------|--------|
| T0.1 QName 缓存 | 2-5s | 500ms→50ms | ⭐⭐⭐ 实测驱动 |
| T0.2 Levenshtein banding | 3-8s | 0.2-0.5s | ⭐⭐ 理论确定 |
| T0.3 by_name 预索引 | 1-3s | 0.1-0.3s | ⭐⭐ 理论确定 |
| T0.5 批量 DELETE | 0.5-1s | <0.1s | ⭐⭐ lazy 路径 |

**T1 额外收益**（基于 commit d6c9517b 已验证的批量写入效果推算）：

| T1 优化 | TS 收益(估) | 确定性 |
|---------|------------|--------|
| T1.1 批量 INSERT 扩展 | 3-6s（DB Write 减少 30-50%） | ⭐⭐⭐ 有实证 |
| T1.4 索引方案 | 1-2s（构建加速 + 内存减少） | ⭐⭐ |
| T1.3 Query 缓存 | 0.5-1s（提取加速） | ⭐⭐ |

### 激进估计（T0 + T1 + T2 仅全量路径）

| 项目 | 当前 | T0+T1+T2 后 | 改进 |
|------|------|------------|------|
| TS monorepo | 75s | 35-40s | -47% to -53% |
| C | 25.5s | 14-17s | -33% to -45% |

T2 核心收益来自 B-T2.1（文件本地预解析减少 20-40% 的全局解析量）+ B-T2.2（内存全局解析消除 DB 读写往返）。

### 增量同步估计

| 场景 | 当前(估) | T0+T1 后 | T0+T1+T2.1 后 |
|------|---------|----------|---------------|
| 1 文件变更 | ~500ms | ~350ms | ~250ms |
| 10 文件变更 | ~2s | ~1.3s | ~0.9s |
| 100 文件变更 | ~8s | ~5s | ~3.5s |

### Rust 项目异常

Rust 项目 104 文件 53.7s 的异常表现需要单独调查。如果确实是 stdlib 重新索引导致，需要优化 stdlib 的增量检测或缓存策略。这可能是独立于 Resolution 优化的另一类问题。

### 最大不确定性

**T0.2 (Levenshtein banding) 的实际收益完全取决于 `fuzzy_search` 在当前被调用的频率和每次调用的候选集大小**。没有 tracing 数据，3-8s 的范围从"几乎没用"（如果大部分被 S2/S4 命中）到"改变游戏规则"（如果大量进入 S6 且缓存 miss 率高）都有可能。

**这就是为什么 T0.0（tracing）必须在所有其他优化之前完成。**

### Profiling Findings（2026-06-10，Atlas self-index）

| 区域 | 耗时 | 占比 |
|------|------|------|
| 不可见 | ~23s | ~59% |
| S6 (fuzzy) | 4,912ms | 12.6% |
| fuzzy_search | 2,731ms | 7.0% |
| S4 (same-file) | 2,307ms | 5.9% |
| Phase 2 (write) | 1,740ms | 4.5% |
| Step B (parallel) | 1,740ms | 4.5% |

关键发现：
- Resolution 占 traced 时间的 ~87%
- S6 + fuzzy_search = 55%+ 的 resolution 时间——这是最高优先级的优化目标
- DB Write 仅占 ~5%——批量 INSERT 的端到端影响有限
- ~59% 不可见时间意味着管线级别的 span 缺失（正在解决中）

---

## Part D: 实施顺序与验证

### Phase 0: Profiling + Benchmark（1 天）

```
1. 添加所有 A1 中的 tracing span
2. 对 Atlas 自身 + TS project 运行全量索引
3. 对每个测试项目运行增量同步（1/10/50 文件修改）
4. 收集 profiling 数据，生成报告
5. 建立 benchmark 基线
6. 决策门：根据 profiling 调整 T2/T3 优先级
```

### Phase 1: T0 优化（已完成 ✅）

```
1. T0.1 — ImportResolver QName 缓存 ✅ 已实现
2. T0.2 — Levenshtein banding ✅ 已实现
3. T0.3 — ResolutionContext by_name 预索引 ✅ 已实现
4. T0.5 — replace_dataflow_for_unit 批量 DELETE ✅ 已实现
5. 验证：pipeline_equivalence 测试 + 性能对比
```

> T0.4 (SummaryBuilder O(N²)) 经代码调查确认 O(N²) 不存在，已移除。

### Phase 2: T1 优化（3-5 天）

```
1. T1.1 — 批量 INSERT 扩展（dataflow_edges + edges ✅ 已实现，其余待推进）
2. T1.2 — Cleanup 事务包裹
3. T1.3 — Query 缓存
4. T1.4 — GlobalSymbolIndex 索引方案
5. T1.5 — GraphBuilder symbol cache 批量加载
6. 验证：端到端性能 + 内存峰值
```

### Phase 3: T2 优化（决策门后，5-7 天）

```
1. 决策门：profiling 确认文件本地预解析的增益
2. T2.1 — 文件本地预解析（全量 + 增量路径均受益）
3. 决策门：profiling 确认全量索引中 DB 往返开销
4. T2.2 — 内存全局解析（仅全量路径）
5. T2.3 — container_id 预计算
6. 验证：全量索引 + 增量同步 + pipeline_equivalence
```

### Phase 4: T3 优化（按需）

```
1. 决策门：fuzzy_search miss_rate > 30%
2. T3.1 — 模糊搜索候选过滤或 Trie
```

---

## Part E: 约束变更清单

| 约束 | 变更 | 受益 |
|------|------|------|
| Extraction 不做 resolution | 允许文件本地预解析（S1+S4，不跨文件） | 20-40% 引用免费解析 |
| Pipeline 阶段分离 | 全量索引中 Resolution 嵌入 Extraction/Write 之间 | 消除 DB 往返 |
| "Resolution does not delete refs" | Refs 一次性写入 resolved_symbol_id | 消除 UPDATE pass |
| container_id 两步写入 | bulk_write 下单步 | 减少 20-30% symbol 写入 |

**不变约束**：
- DB Schema V1 ✓
- blake3 确定性 ID ✓
- Lazy index 层级 ✓
- 模块边界（crate 依赖方向）✓
- MCP 契约 ✓

> **注意**：T2.1 约束解读需要显式修改 `architecture.md`，而非重新解释现有措辞。
