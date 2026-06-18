# Atlas Performance Baseline

## Test Environment

- **Machine**: Apple Silicon (aarch64), macOS
- **Atlas build**: `cargo build --release -p atlas-cli`
- **Date**: 2026-05-23 (Baselines 1-2), 2026-06-10 (Baseline 3), 2026-06-11 (Baseline 4), 2026-06-12 (Baseline 5), 2026-06-18 (Baseline 6)

## Baseline 1: TypeScript Project (project-graph)

### Project Profile
- **Files**: 165 discovered, 146 indexed
- **Languages**: TypeScript (183), JavaScript (1)
- **Symbols extracted**: 1,704
- **References extracted**: 11,819
- **Resolution rate**: 78.7% (9,299 resolved / 2,520 unresolved)

### Phase Timings

| Phase | Time | % of Total | Notes |
|-------|------|-----------|-------|
| Discovery | 12ms | 0.1% | 165 items |
| Hash check | 3ms | 0.0% | 0 reused, 165 dirty |
| Parse/extract | 592ms | 6.4% | avg 3.6ms/file |
| DB write | 2,190ms | 23.6% | 146 items inserted |
| Resolution | 5,957ms | **64.3%** | 11,819 refs → 9,299 resolved |
| Graph build | 496ms | 5.4% | 9,708 edges |
| **Total** | **9,264ms** | 100% | |

> **Status**: Historical baseline from pre-optimization code. Resolution breakdown below uses the old strategy names (`fuzzy_match`, `name_only`). See Baseline 3 for current strategy distribution with S1-S6 naming.

## Baseline 2: Multi-Language Project (Atlas Itself)

### Project Profile
- **Files**: 156 indexed
- **Languages**: 11 (Rust 129, Go 4, C# 4, Kotlin 4, PHP 4, Ruby 4, TypeScript 3, ArkTS 1, C 1, JavaScript 1, Python 1)
- **Symbols extracted**: 5,065
- **References extracted**: 27,786
- **Resolution rate**: 74.8% (20,779 resolved / 7,007 unresolved)

### Phase Timings

| Phase | Time | % of Total | Notes |
|-------|------|-----------|-------|
| Discovery | 13ms | 0.0% | 156 items |
| Hash check | 3ms | 0.0% | |
| Parse/extract | 253ms | 0.9% | avg 1.6ms/file |
| DB write | 2,207ms | 7.9% | 156 items |
| Resolution | 22,300ms | **79.4%** | 27,786 refs → 20,779 resolved |
| Graph build | 3,367ms | 12.0% | 19,618 edges |
| **Total** | **28,100ms** | 100% | |

### Per-Language Parse/Extract Speed
| Language | Files | Time | avg/file |
|----------|-------|------|----------|
| Rust | 129 | 1,922ms | 14.9ms |
| Ruby | 4 | 236ms | 59ms |
| Kotlin | 4 | 233ms | 58ms |
| C# | 4 | 213ms | 53ms |
| TypeScript | 3 | 118ms | 39ms |
| PHP | 4 | 75ms | 19ms |
| JavaScript | 1 | 34ms | 34ms |
| ArkTS | 1 | 21ms | 21ms |
| Python | 1 | 17ms | 17ms |
| Go | 4 | 9ms | 2.3ms |
| C | 1 | 7ms | 7ms |

> **Status**: Historical baseline. The FK constraint issue in Baseline 1 (19 failed files) has since been resolved.

---

## Baseline 3: TypeScript Monorepo (Optimized)

### Project Profile
- **Files**: 1,931 indexed (TypeScript)
- **Symbols extracted**: 35,080
- **References extracted**: 313,985
- **Edges built**: 65,350 (structural analysis only)

### Performance (release build, RUST_LOG=warn, clean .atlas/)

| Metric | Pre-Optimization | Post-Optimization | Improvement |
|--------|-----------------|-------------------|-------------|
| Wall clock | 53.29s | 18.82s | **64.7%** |
| CPU utilization | 232% | 713% | 3.1x higher throughput |
| Resolution S5 time (cumulative) | 252.80s | 51.11s | **79.8%** reduction |
| Resolution S6 time (cumulative) | 87.66s | 48.19s | 45.0% reduction |

### Resolution Strategy Distribution (313,985 references)

| Strategy | Count | % | Per-Call Cost | Status |
|----------|-------|---|---------------|--------|
| **S1** — Builtin filter | 7,616 | 2.4% | ~2.5μs | Skip common names (error, console, etc.) |
| **S2** — Scope-local | 24,462 | 7.8% | ~3.6μs | Exact match in local scope tree |
| **S3** — Container-local | 0 | 0.0% | — | Dead code for TypeScript |
| **S4** — Same-file name | 31,113 | 9.9% | ~1.0μs | Hash map lookup, O(1) |
| **S5** — Import resolution | 26,716 | 8.5% | ~2.0ms | **Dominant CPU bottleneck** (53% of cumulative) |
| **S6** — Global + fuzzy | 176,860 | 56.3% | ~271μs | Largest call volume, but cheapest per call |
| **MISS** — Unresolved | 47,218 | 15.0% | — | Traverses all 6 strategies to None |

### S6 Internal Breakdown (176,860 calls)

| Sub-strategy | Count | % of S6 | Method |
|-------------|-------|---------|--------|
| Exact name match | 153,804 | 87.0% | `HashMap::get` O(1) |
| Fuzzy proximity | 22,738 | 12.9% | Directory-scoped trigram + banding |
| Fuzzy global | 318 | 0.2% | Full 35K-symbol scan |

The proximity filter eliminates 99.8% of global fuzzy scans. A full trigram index for global fuzzy search would optimize only 318 calls per run — essentially worthless.

---

## Baseline 4: Elasticsearch (Large-Scale DB Write Benchmark)

### Project Profile
- **Files**: 30,059 indexed
- **Languages**: Java, TypeScript, JavaScript, Python, Go, and others
- **Symbols extracted**: 741,848
- **References extracted**: 6,790,151
- **Callsites extracted**: 3,992,876
- **Imports extracted**: 485,254
- **Scopes extracted**: 1,222,940

### DB Write Phase — Per-Table Timing (debug build, after optimizations)

| Table | Time (ms) | % of DB Write |
|-------|-----------|---------------|
| commit | 243,856 | 60.6% |
| references | 65,888 | 16.4% |
| callsites | 49,884 | 12.4% |
| symbols | 15,487 | 3.8% |
| scopes | 13,136 | 3.3% |
| imports | 8,091 | 2.0% |
| bindings | 6,312 | 1.6% |
| **Total** | **402,654** | 100% |

### Chunk Distribution (weight-budget, max_weight=1,000,000)

| Metric | Value |
|--------|-------|
| Total chunks | 154 |
| Slow chunks (≥1s) | 56 |
| Slow chunk p50 | 3,888ms |
| Slow chunk p90 | 5,522ms |
| Slow chunk p95 | 5,929ms |
| Slow chunk max | 6,513ms |
| Batch failures | 0 |

### Chunking Model Comparison: Fixed 500 vs Weight-Budget

| Metric | Fixed 500-file | Weight-Budget (1M) | Improvement |
|--------|---------------|-------------------|-------------|
| Max chunk time | 52.1s | 25.7s | **−50.7%** |
| Tail shape | Multiple 25–52s spikes | All chunks ≤ 26s | Extreme tail eliminated |

Weight-budget chunking replaced the fixed 500-file grouping with a `max_weight` budget of 1,000,000 (file weight = symbol count + reference count + callsite count). This prevents a single chunk from containing many symbol/reference-dense files, which was the root cause of the 50s+ tail spikes in the fixed-size model. The improvement is entirely in tail latency; total DB write time is similar because the same data must still be committed.

### Active Optimizations (verified zero regression)

| Optimization | Mechanism | Effect |
|-------------|-----------|--------|
| Import resolution caches | 3x `thread_local! RefCell<HashMap>` (QName, reexport chain, module path) | S5 cumulative 252.8s → 51.1s |
| Levenshtein bounded distance | Early termination when row min > max_dist | +2.6s benefit (verified by revert) |
| Proximity-aware fuzzy search | Directory-scoped candidate pool before global fallback | 99.8% reduction in global scans, +289 edge matches |
| `fuzzy_cache` / `proximity_cache` | `thread_local! RefCell<HashMap>` (was `Mutex`) | Eliminated Mutex contention |
| Strategy counters and timers | 7 `AtomicU64` counters + 6 nanosecond timers | Profiling infra, zero overhead |
| **P0: project-level generation skip** | Skip resolution/graph when no files changed + config unchanged | No-op re-run: 33s → 0.22s (150x) |
| **P2: callsite batch backfill** | Collect `(ref_id, callee)` pairs in Phase 2, single batch UPDATE | Eliminates 9,372 per-edge UPDATEs |
| **P3: per-file resolution fingerprint** | `content_hash` in `extraction_state`; clean files skip Step A context build | 27.34s → 25.99s (−5%) |
| **T1.2: cleanup transaction wrapping** | Single transaction for stale file cleanup (was 3N+1 transactions) | Atomic cleanup, no partial state |
| **Deferred index creation (bulk-load)** | Drop all non-PK indexes before write, recreate after. FTS rebuilt at Phase 10. | 25.81s → 18.82s (−27%) |
| **S6 exact direct target selection** | Resolve exact global name target inside `GlobalSymbolIndex` without cloning/sorting candidate Vec or running `NameMatcher` again | Kept after Elasticsearch short-window validation; full-run gain still TBD |
| **P4: COUNT(*) progress load** | Replace `find_unresolved_references()` full Vec materialization with index-only `COUNT(*)` — leverages existing partial index `idx_references_unresolved` | progress_total_load_ms: 39ms → 5ms (−87.2%); ES-scale: estimated ~20s → <1ms |
| **P5: shared `get_all_symbols()`** | Single `get_all_symbols()` call shared between `GlobalSymbolIndex::build()` and `GraphBuilder` symbol_override; `ResolutionSession::build_from_symbols()` + `resolve_all_parallel_with_symbols()` API | session_build_ms: 17ms → 8ms (−52.9%); graph_symbol_load_ms: 11ms → 9ms (−18.2%) |
| **P6: import-scoped S6 pre-filtering** | `GlobalSymbolIndex::find_exact_name_target_in_scope` prioritises candidates from files reachable via the current file's import graph before falling back to global scan; `ResolutionContext::preferred_file_ids` populated from `store.find_files_by_path_prefix` per import | 0 correctness regression (identical resolved_refs & edges_built); slight overhead on Rust projects (+22ms context build); expected gain on import-heavy TS/Java monorepos with high-fanout names |

### Rejected Optimizations (verified regression)

| Attempt | Expected | Actual | Root Cause |
|---------|----------|--------|------------|
| Mutex-guarded QName cache | Shared cache higher hit rate | **+5.8s** | Mutex contention at 249% CPU |
| Dynamic SQL batch INSERT | Fewer DB round-trips | **+22.8s** | SQLite prepared statement cache > dynamic SQL |
| Subquery batch DELETE | Single statement instead of N+1 | **+330s** (full analysis) | Missing index → full table scan on subquery |
| Batch symbol lookup (IN clause) | One query instead of N | **+2.2s** | Same prepared-statement advantage as INSERT |
| Debug tracing spans on hot path | Low overhead | **+16%** wall clock | Span enter/exit cost at 19K calls/sec |
| In-memory symbol pre-load | Avoid DB queries | Neutral (wall clock + higher CPU) | SQLite query is already cached |
| `has_name` fast-path | Skip hash map lookups | Neutral | Extra lookup cost = saved cost |
| `find_by_name_refs` clone elimination | Fewer allocations | Marginal (0.2s) | Clone cost is negligible on modern hardware |

**Key pattern**: SQLite's internal prepared statement cache makes per-row `stmt.execute(params![])` extremely efficient. Dynamic SQL construction (building variable-length `INSERT INTO ... VALUES` statements) defeats this cache entirely. This is counter-intuitive for engineers coming from PostgreSQL or MySQL where batching is almost always a win.

---

## Baseline 6: Atlas Self-Index A/B — Resolution Pipeline Optimizations

### Project Profile
- **Files**: 156 discovered, 327 files with references
- **Languages**: Rust (129), Go (4), C# (4), Kotlin (4), PHP (4), Ruby (4), TypeScript (3), ArkTS (1), C (1), JavaScript (1), Python (1)
- **References extracted**: 100,499
- **Resolution rate**: 79.5% (79,883 resolved / 20,616 unresolved)

### A/B Comparison: Baseline vs P4+P5+P6

| Metric | Baseline | Optimized | Change | Notes |
|--------|----------|-----------|--------|-------|
| `progress_total_load_ms` | 39ms | **5ms** | **−87.2%** | P4: COUNT(*) replaces full Vec materialization |
| `session_build_ms` | 17ms | **8ms** | **−52.9%** | P5: single `get_all_symbols()` shared |
| `graph_symbol_load_ms` | 11ms | **9ms** | **−18.2%** | P5: no second `get_all_symbols()` for GraphBuilder |
| `context_build_ms` | 40ms | 62ms | +55.0% | P6: `preferred_file_ids` computation overhead |
| `resolve_all_parallel_ms` | 729ms | 796ms | +9.2% | |
| `graph_build_ms` | 373ms | 364ms | −2.4% | |
| `resolved_refs` | 72,349 | 72,349 | **0%** | Correctness: identical |
| `edges_built` | 72,252 | 72,252 | **0%** | Correctness: identical |

### Per-Optimization Attribution

| Optimization | Wall Impact | Cumulative Impact | Mechanism |
|-------------|-------------|-------------------|-----------|
| **P4: COUNT(\*) progress load** | −87.2% progress materialization | ES-scale: ~20s → <1ms | Leverages existing `idx_references_unresolved` partial index for index-only scan |
| **P5: shared `get_all_symbols()`** | −52.9% session build | Eliminates 1 redundant symbols DB load | `GlobalSymbolIndex::build_from_symbols()` + `ResolutionSession::build_from_symbols()` + `resolve_all_parallel_with_symbols()` API |
| **P6: import-scoped S6 pre-filtering** | +9.2% total (Rust project) | Expected gain on import-heavy TS/Java monorepos | `ResolutionContext::preferred_file_ids` built from `import.module → find_files_by_path_prefix`; gated at candidate count >= 50 |

### P6 Deep Dive

P6 adds `ResolutionContext::preferred_file_ids: HashSet<FileId>` populated during
`ResolutionContext::build()` by resolving each import's module path against the
store file table. The `GlobalSymbolIndex::find_exact_name_target_in_scope()`
method first scans only preferred-file candidates before falling back to the
full global scan.

On the Atlas project (Rust, sparse imports), `preferred_file_ids` is typically
empty or small, producing a net **+22ms context build** overhead with no S6
reduction. On import-heavy TS/Java monorepos with high-fan-out names
(e.g. `builder` at 259M candidate pairs in Elasticsearch), the import-scoped
pre-filtering is expected to reduce S6 scan volume proportionally to the
candidate's locality within imported modules.

**Design rationale**: P6 is a receiver-type heuristic without requiring full
LSP — it uses import paths already extracted by the language adapter as a proxy
for "type reachability". The gate at `candidates.len() >= 50` ensures the
two-pass scan is only activated when the candidate set is large enough for the
preferred-file scan to outpace the overhead.

### Lessons

1. **P4 vindicates the SQLite partial index strategy**: the existing
   `idx_references_unresolved` index (created at schema init, never dropped)
   allows an index-only `COUNT(*)` scan.  The previous code materialized
   a full `Vec<ReferenceUse>` (6.8M rows on ES) solely for a progress-bar
   total — a pattern that persisted because the resolution phase internally
   also needs the full list.  The orchestrator should never duplicate
   large materializations that the downstream phase must do anyway.

2. **P5 demonstrates that shared-pipeline callers should pre-load once**:
   `phase_resolve_and_build` already needed `get_all_symbols()` for
   GraphBuilder's `symbol_override`.  `GlobalSymbolIndex::build()` was
   calling it again internally.  The fix was a variant constructor
   (`build_from_symbols`) + a new resolver entry point
   (`resolve_all_parallel_with_symbols`), keeping the original public API
   untouched.

3. **P6 is sensitive to project structure**: the import-scoped
   pre-filtering overhead is proportional to the number of imports per file
   and the DB cost of `find_files_by_path_prefix`.  For Rust/C/C++ projects
   with few imports (or include-style imports that don't resolve through
   the same prefix-matching heuristic), the overhead dominates.  For
   TypeScript/Java projects with dense import graphs, the savings on S6
   high-fan-out names should outweigh the context build cost.

---

### 做了什么（17 项已提交优化）

从 53.29s → 18.82s（−64.7%），收益来自五个层次：

| 层次 | 优化项 | 收益 | 核心手段 |
|------|--------|------|---------|
| **缓存层** | QName / reexport / module_path 三个 Mutex 缓存 | −32.5% | S5 import resolution 是隐藏瓶颈（74% CPU），缓存消除重复 DB 查询 |
| **写入层** | 延迟索引创建（bulk-load deferred indexes）| −27.1% | 写入时只维护 PK，查询索引和 FTS 全部延后到 Phase 10 重建 |
| **架构层** | P0 generation skip + P2 callsite batch + P3 per-file fingerprint + T1.2 cleanup tx | −19.1% | 改变"哪些工作不需要做"，而非"已有工作如何更快" |
| **写入模型层** | weight-budget chunking + symbol 去重 + hot-table insert-only | max −50.7%，commit −33.8%，p95 −45.7% | 削峰：固定 chunk→按权重组；减写放大：去重 symbol 26%，热表避免 OR REPLACE |
| **算法层** | Levenshtein banding + S6 proximity 优先搜索 | −2.7% | 代码改动小（~40 行），收益确定但绝对值有限 |
| **消除浪费层** | P4 COUNT(\*) + P5 shared get_all_symbols() + P6 import-scoped S6 | progress load −87.2%，session build −52.9% | 消除 orchestrator 双物化 + get_all_symbols() 重复调用 + S6 候选集缩减 |

> **写入模型层明细**（Elasticsearch 30K-file benchmark，debug build）：
> - **Weight-budget chunking**：固定 500-file → max_weight=1,000,000，max chunk time 52.1s → 25.7s（−50.7%）。尾部从多个 25–52s 尖刺收敛至全部 ≤ 26s。
> - **Symbol 去重**：chunk 内按 symbol_id last-write-wins，跳过 261,731 次重复写入（symbols_attempted=1,003,579，实际 741,848，26% 为重复）。
> - **Hot-table insert-only**：references / callsites 在批量路径（batch.len() > 1）改 plain INSERT，避免 INSERT OR REPLACE 的冲突检查和 delete-insert 放大。对比去重基准线：references_ms 87,448 → 65,888（−24.7%），callsites_ms 65,529 → 49,884（−23.9%），commit_ms 368,507 → 243,856（−33.8%），慢 chunk p95 10,910ms → 5,929ms（−45.7%）。

### 不能做什么（11 项已验证不可行）

| 优化项 | 回归量 | 根因 |
|--------|--------|------|
| T0.5 子查询批量 DELETE | +330s | 子查询对无索引列做全表扫描 |
| T1.1 动态 SQL 批量 INSERT | +22.8s | SQLite prepared stmt cache > 动态 SQL |
| T1.5 批量 symbol 查询 | +2.17s | 同 T1.1 |
| T0.1 Mutex QName 缓存 | +5.8s | 并发锁竞争（249% CPU） |
| Hot-path debug_span | +16% | 19K 调用 × per-span 开销 |
| Symbol preload | 中性 | SQLite 内部缓存已足够快 |
| T0.3 by_name HashMap | 中性 | S4 的 O(F) 中 F < 100，微秒级 |
| has_name 快速路径 | 中性 | 额外 HashMap 查询抵消了收益 |
| BEGIN EXCLUSIVE 替代 BEGIN IMMEDIATE | 不显著 | WAL 模式下差异有限，p95/max 无稳定改善 |
| PRAGMA cache_spill=OFF | 恶化尾部 | dirty page 压力推迟到后半段/commit，尾部更差 |
| Chunk-level 延迟写 references/callsites | 恶化尾部 | 表间重排未减少写放大，尾部明显恶化 |
| S6 exact-case extra index | Neutral/negative | Extra index memory and lookup path did not reduce S6 CPU on Elasticsearch |
| S6 high-fanout shared cache | Negative | Mutex/cache overhead exceeded saved candidate scans |
| S6 per-file exact memo | Neutral/negative | Same-file repeated-name locality was insufficient; HashMap maintenance erased benefit |
| S6 tier-0 exact early return | Neutral/negative | Candidate ordering did not make early exits frequent enough |

**核心教训**：
1. **SQLite 的 prepared statement 缓存比动态 batch SQL 更快**——不要假设"批量一定比逐行快"。
2. **SQL 改动前必须 EXPLAIN QUERY PLAN**——T0.5 的 +330s 回归来自无索引列的全表扫描。
3. **高频路径的 tracing span 有隐性成本**——即使用 `debug_span!`（warn 级别下零开销），enter/exit 仍消耗 CPU。
4. **小数据集的算法复杂度优化收益可忽略**——S4 的 O(F) 中 F 通常 < 100。
5. **SQLite PRAGMA 调参不是银弹**——cache_spill、事务模式等调整在 bulk write 场景下效果不稳定或负面，应先优化写入模型本身。
6. **写入重排不能替代写放大消除**——延迟写、表间调序不减少实际写入量，应优先减少不必要的 SQL 操作（如 OR REPLACE 冲突检查）。

---

## Baseline 5: Elasticsearch Resolving refs Investigation

### Context

After DB write optimization, the next visible large-scale bottleneck moved to
`Resolving refs` on the Elasticsearch example. The investigation used the
existing extracted Elasticsearch `.atlas` database and repeatedly reset only the
resolution/edge state:

```sql
UPDATE "references"
SET resolved_symbol_id=NULL,
    resolved_confidence=NULL,
    resolved_strategy=NULL,
    resolved_provenance=NULL;
DELETE FROM symbol_edges;
DELETE FROM project_metadata
WHERE key IN ('resolution_config_hash',
              'resolution_generation_version',
              'graph_generation_version');
DELETE FROM extraction_state WHERE layer='resolution';
```

This keeps extraction and DB-write facts stable while making each resolution
run start from `6,790,151` unresolved references and `0` edges.

### Observability Added

Resolution now emits periodic `resolution.progress` records during Step B:

| Field | Why it matters |
|-------|----------------|
| `scanned_refs` / `scan_refs_per_sec` | Resolution-side throughput, including misses |
| `matched_refs` / `match_rate_pct` | How much work becomes writer input |
| `writer_rows_written` / `write_refs_per_sec` | DB writer throughput |
| `queued_resolved` | Backpressure indicator between resolver and writer |
| `dirty_files_done` / `clean_files_done` | File-level progress and skew |
| `s5_count`, `s5_time_s`, `s6_count`, `s6_exact`, `s6_time_s` | Live strategy attribution without waiting for final summary |

The CLI progress bar also tracks scanned references instead of resolved rows.
This avoids under-reporting progress on workloads with many misses.

### Experimental Facts

All short-window tests below were debug builds on the same Elasticsearch DB
state. They are used for directional bottleneck isolation, not for release-mode
end-to-end claims.

| Observation | Evidence | Conclusion |
|-------------|----------|------------|
| DB writer was not the primary `Resolving refs` bottleneck | `queued_resolved` stayed around 0-2K while the bounded channel capacity is 4K; `writer_rows_written` closely followed `matched_refs` | Continue optimizing resolver CPU before SQLite writer |
| S6 dominated CPU, not S5 | At ~90s Step B: `s6_time_s≈1068s`, `s5_time_s≈7.2s` | Import-resolution caches already solved S5 for this workload |
| S6 exact dominated S6 | At ~90s Step B: `s6_exact=33,403` out of `s6_count=34,267` | Optimize exact global name matching before fuzzy |
| Progress-total duplicate load is measurable but not dominant | `progress_total_load_ms` was 13-20s on a 6.79M-ref DB | Worth cleaning later, but far smaller than S6 CPU |
| High-fanout names explain S6 cost | SQL fanout examples: `builder` 83,282 refs × 3,113 symbols = 259M candidate-pairs; `get` 95,906 × 1,457 = 140M; `request` 44,386 × 3,090 = 137M | Name-only resolution on common identifiers is the structural problem |

High-fanout query used:

```sql
WITH sc AS (
  SELECT lower(name) n, COUNT(*) symbols
  FROM symbols
  GROUP BY lower(name)
),
rc AS (
  SELECT lower(name) n, COUNT(*) refs
  FROM "references"
  GROUP BY lower(name)
)
SELECT rc.n, refs, symbols, refs * symbols AS fanout
FROM rc JOIN sc USING(n)
ORDER BY fanout DESC
LIMIT 30;
```

Top fanout examples:

| Name | Refs | Symbols | Refs × Symbols |
|------|------|---------|----------------|
| `builder` | 83,282 | 3,113 | 259,256,866 |
| `get` | 95,906 | 1,457 | 139,735,042 |
| `request` | 44,386 | 3,090 | 137,152,740 |
| `tostring` | 18,187 | 4,072 | 74,057,464 |
| `name` | 22,044 | 3,221 | 71,003,724 |

### Kept Change

`S6 exact direct target selection` was kept and committed as:

```
79742181 perf(resolution): observe and streamline S6 exact matching
```

Mechanism:

- Move exact global-name target selection into `GlobalSymbolIndex`.
- Avoid cloning the whole candidate Vec from `find_by_name()` /
  `find_by_name_proximity()`.
- Avoid running `NameMatcher::best_match()` on candidates that are already
  lower-name matches.
- Preserve semantics: exact-case matches beat case-insensitive matches; within
  the same confidence class, directory proximity breaks ties.

Tests added/kept:

- `exact_name_target_prefers_exact_case_before_proximity`
- `exact_name_target_uses_proximity_within_same_confidence`
- `resolved_callsites_consistent_across_resolution_paths`

### Rejected Attempts

Each attempt was implemented, compiled, run on the Elasticsearch DB, then
reverted when the short-window evidence did not support keeping it.

| Attempt | Expected | Short-window result | Decision |
|---------|----------|---------------------|----------|
| Exact-case side index | Avoid `to_lowercase()` allocation for exact-case hits | ~180s Step B: `scanned_refs=103,498` vs previous direct path `108,227`; no S6 improvement | Reverted |
| High-fanout shared cache keyed by `(name,file)` | Avoid repeated scans for names with ≥64 candidates | ~80s Step B: `scanned_refs=43,305` vs direct path `50,102`; S6 time unchanged | Reverted |
| Per-file S6 exact memo | Avoid shared mutex while caching repeated names inside one file | ~90s Step B: `scanned_refs=55,149` vs direct path `58,567`; no S6 time improvement | Reverted |
| Tier-0 exact early return | Stop scanning once same-directory exact-case candidate is found | ~80s Step B: `scanned_refs=46,905` vs direct path `50,102`; no S6 time improvement | Reverted |

### Lessons

1. **Large-project S6 is a different workload than Baseline 3 S6.** In the
   TypeScript monorepo, global fuzzy was already almost eliminated. In
   Elasticsearch, S6 exact name-only matching is the dominant CPU consumer
   because common identifiers have thousands of symbols.
2. **Backpressure fields prevent false DB blame.** Without `queued_resolved`
   and `writer_rows_written`, it is easy to blame SQLite when the real issue is
   that the resolver is not feeding the writer fast enough.
3. **Caches are not automatically good.** Shared caches introduced contention;
   per-file caches lacked enough locality. Both were plausible from code review
   and rejected by measurement.
4. **Micro-optimizations are noisy at this scale.** Extra indexes, early exits,
   and memoization changed local code shape but did not move S6 cumulative time.
   The next meaningful optimization likely needs to reduce the candidate set for
   ambiguous names rather than make the same scan slightly cheaper.
5. **Do not extrapolate from short-window tests to final wall-clock claims.**
   Short runs are useful to reject regressions and locate bottlenecks. Any kept
   performance claim still needs a clean release-mode full run.

### 没做什么（5 项推迟）

| 方向 | 预估收益 | 推迟原因 |
|------|---------|---------|
| **T2.1 文件本地预解析** | 20-40% 引用在 extraction 阶段解析 | 需修改 architecture.md 约束（"不做跨文件 resolution"→允许同文件 resolution） |
| **T3.1 BK-Tree 模糊索引** | S6 fuzzy_prox 从 O(候选集) 降至 O(26²×L) | 当前候选集约 50，收益有限 |
| **Step A 并行化** | <0.5s | P3 已将 Step A 减至单文件 context build |
| **P4 path prefix range query** | <0.5s | REEXPORT/MODULE_PATH 缓存已覆盖 99% 查询 |
| **增量同步优化** | 场景依赖 | T1.2 已为增量清理铺路，但增量 resolution 路径仍需优化 |

---

## Key Findings

### 1. The real bottleneck was import resolution, not fuzzy matching

Pre-optimization strategy distribution suggested fuzzy matching was 73% of the bottleneck. Strategy timing counters revealed the opposite: **S5 (import resolution) was 74% of CPU time** despite being only 8.5% of calls — each import resolution cost ~9.4ms via recursive DB queries through reexport chains. Adding three `thread_local!` caches (QName, reexport chain, module path) reduced S5 cumulative time from 252.8s to 51.1s.

**Lesson**: Call count distribution alone is misleading. Always add per-strategy timing before prioritizing. What looks like a volume problem may be a per-call cost problem.

### 2. S6 (global+fuzzy) was already well-optimized

After banding + proximity filtering, global fuzzy search triggers only 318 times out of 176,860 S6 calls (0.2%). A full trigram index or BK-tree for global fuzzy would optimize less than 0.2% of the workload. The proximity directory filter (search same-directory symbols first) eliminated the need for a global index.

### 3. DB write is cheap at small scale, expensive at large scale — and fixable

At 1,931 files (~35K symbols, ~314K refs), extraction and DB writes combined account for <20% of wall clock when indexes are deferred. However, on a 14G production DB with 13.8M references, `references` 100K-row inserts cost 21.9s with all indexes online — a 150x slowdown vs writing to an index-less table. The bottleneck is B-tree index maintenance on the 4 `references` secondary indexes. **Deferring index creation until after bulk write (Phase 10)** eliminates this cost: drop all non-PK indexes and FTS triggers before extraction, write with only PK constraints, recreate indexes and rebuild FTS at finalize. TS project: 25.81s → 18.82s (−27.1% even at 35K-symbol scale).

### 4. New language extraction is fast and reliable

All 6 new DataflowBasic languages extracted correctly with 0 errors. Parse/extract speeds: Go 2.3ms/file, Rust 14.9ms/file, Ruby 59ms/file. Tree-sitter grammars are mature enough for production use.

### 5. 1,931-file TypeScript monorepo indexes in ~18.8 seconds

Acceptable for batch/CI use. The remaining optimization space (<10% further improvement) requires structural changes: extraction-resolution pipeline fusion, incremental index path optimization, or language-specific strategy tuning. These are diminishing returns compared to the 64.7% already achieved.

### 6. Checkpoint is rarely the bottleneck in fresh bulk writes

On the Elasticsearch 30K-file fresh index, 48 PASSIVE checkpoints all completed with 0ms elapsed and 0 remaining frames. The final checkpoint truncate took only 18-21ms. This definitively rules out WAL/checkpoint as a bottleneck for fresh bulk writes — the real bottleneck is transaction commit (SQLite dirty page flush). Adding checkpoint observability before optimizing checkpoint is essential: without it, you'd waste time tuning `wal_autocheckpoint` or `checkpoint_threshold` for a non-problem.

### 7. Commit dominates large-scale SQLite writes; reducing write amplification beats tuning commit

On Elasticsearch 30K-file DB write, `commit_ms` accounted for 60.6% of total DB write time (243,856ms / 402,654ms). The largest chunk spent 92.8% of its time in `tx.commit()`. Tuning the commit mechanism itself (PRAGMA cache_spill, transaction mode) was strictly inferior to reducing what gets committed — see §8 and Methodology §10 for the insert-only approach that cut commit_ms by 33.8%.

### 8. INSERT OR REPLACE has hidden write amplification in bulk-clean-then-write paths

When data is bulk-cleaned before write (stale file data deleted, then fresh data inserted), there should be zero primary key conflicts. But `INSERT OR REPLACE` still performs a conflict check (B-tree lookup) on every row, and if SQLite's internal state suggests a possible conflict, it does a delete-before-insert cycle — doubling the B-tree work. Switching to plain `INSERT` in the batch path eliminated this overhead:

| Metric | OR REPLACE (baseline) | Plain INSERT | Change |
|--------|----------------------|-------------|--------|
| references_ms | 87,448 | 65,888 | **−24.7%** |
| callsites_ms | 65,529 | 49,884 | **−23.9%** |
| commit_ms | 368,507 | 243,856 | **−33.8%** |
| Slow chunk p95 | 10,910ms | 5,929ms | **−45.7%** |

**Critical constraint**: This optimization is only safe when data has been pre-cleaned. Single-file writes, incremental paths, and fallback paths must keep `INSERT OR REPLACE` semantics. The implementation uses a scoped helper that only activates plain INSERT in the bulk batch path (`batch.len() > 1`). A regression test verifies that repeated `insert_file_facts()` calls with the same data do not cause row duplication or errors on non-batch paths.

---

## Performance Optimization Methodology

> Lessons learned from the 2026-06-10/11 optimization cycle that reduced a 1,931-file
> TypeScript project index from 53.29s to 18.82s (64.7% improvement). These are
> process-level principles, not code-specific recipes.

### 0. The Golden Rule: Measure First, Optimize Never in the Dark

The most expensive mistake in performance work is optimizing the wrong thing.
Before writing a single line of optimization code:

1. **Make the hot path observable.** Add lightweight instrumentation (counters,
   timers, spans) to the critical path. Without this, you're guessing.
2. **Get a release-mode baseline.** Debug builds and tracing-enabled runs have
   overhead that can mask or distort real bottlenecks — see §5.
3. **Profile on a real workload.** A 100-file synthetic test tells you nothing
   about a 2,000-file monorepo. Use the largest project you have.

In our cycle, the initial performance plan identified 10 candidate optimizations
based on static code analysis. Only 4 survived A/B verification. One was
targeting a code path that didn't exist (SummaryBuilder O(N²) — the code had
already been fixed). Two introduced regressions larger than their benefit. The
remaining 3 were neutral. The largest breakthroughs — import resolution caches
(responsible for >50% of the 64.7% improvement) and deferred index creation
(27% improvement on its own) — were **not in the original plan at all**.
Import caches were discovered through strategy timing counters; the bulk-load
approach came from analyzing production DB write behavior on a 14G real index.

### 1. Cumulative CPU Time ≠ Wall Clock Time

In parallel systems, a bottleneck may be invisible in wall clock but obvious in
cumulative CPU. Example from our profiling:

| Strategy | Calls | Wall Impact | Cumulative CPU | % of CPU |
|----------|-------|-------------|----------------|----------|
| S6 (global+fuzzy) | 176,860 | ~12.7s | 48.2s | 47% |
| S5 (import) | 26,716 | ~3.3s | 51.1s | 53% |

Wall clock alone would suggest S6 is the dominant bottleneck (12.7s vs 3.3s).
But cumulative CPU reveals S5 costs more total work — it's just better spread
across threads. After caching, S5 cumulative dropped from 252.8s to 51.1s: the 201.7s
saving was invisible in wall clock because it was parallelized, but it freed
CPU capacity that other work could use.

**Rule**: In any parallel system, always measure both wall clock AND cumulative
CPU (per-thread aggregation). The wall clock bottleneck and the CPU bottleneck
are often different things.

### 2. A/B Testing Discipline

Every optimization claim must survive a clean A/B comparison:

```
Baseline (zero changes) ──→ 53.29s
Proposed change         ──→ 51.84s  ← claim: -2.7%
```

Essential controls:
- **Identical input.** Same project, same source files, same `.git` state.
- **Clean state.** Delete `.atlas/` database before every run. Stale caches
  and incremental paths contaminate measurements.
- **Release build.** `--release` only. Debug mode has bounds checks and no
  inlining; tracing subscribers add per-span overhead (see §5).
- **Minimal logging.** `RUST_LOG=warn` or equivalent. Every `info!()` call
  that hits the subscriber costs time.
- **Run multiple times.** Single-run variance on a live system can be ±5%.
  Look for consistent direction, not absolute numbers.

When a change shows a regression, **verify by reverting** before moving on.
A single misattributed regression can lead to hours of dead-end investigation.

### 3. Progressive Isolation: Revert One Variable at a Time

When multiple optimizations are applied together and the result is a regression,
the fastest way to find the culprit is **progressive revert**:

```
Full optimized:    81.07s  (+13.7% vs baseline)  ← something is wrong
Disable T0.1:      75.23s  (+5.5%)               ← T0.1 costs 5.8s
Disable T0.2:      77.83s  (+9.2%)               ← T0.2 saves 2.6s
Disable T1.1:      52.45s  (-26.4%)              ← T1.1 costs 22.8s!
```

Without this isolation, you'd either discard all optimizations (throwing away
the +2.6s benefit of T0.2) or ship a regression (accepting the +22.8s cost of
T1.1). You'd have no way to know which parts are working.

**Rule**: When benchmarking a group of changes, make each independently
revertible. A boolean flag, a feature gate, or a git stash — whatever lets you
toggle one variable without affecting others.

### 4. Thread-Local Caches Beat Mutex-Guarded Shared Caches

In a parallel system with 8 worker threads and a read-dominated cache:

```
Mutex<HashMap> shared cache:  +5.8s regression (contention at 249% CPU)
thread_local RefCell cache:   zero measurable overhead
```

The intuition that "shared cache has better hit rate" is correct in theory but
wrong in practice for this workload: each rayon thread processes a different
file's references, and import symbols cluster by module (same file, same thread).
Thread-local caches achieve nearly the same hit rate as a shared cache for
import resolution, without any cross-thread synchronization.

**Rule**: Before reaching for a shared cache with locking, measure whether
thread-local caches have sufficient locality for your access pattern. If they
do, the elimination of contention is almost always worth any reduction in hit
rate.

### 5. Tracing Spans Are Not Free

`tracing` spans have runtime cost even when the log level filters them out:

| Configuration | Hot Path Overhead |
|---------------|-------------------|
| No spans | Baseline |
| `debug_span!` on 19K-call path, RUST_LOG=warn | +16% wall clock |
| `info_span!` on 3-call path, RUST_LOG=warn | negligible |

The overhead comes from span creation, field evaluation, and enter/exit hooks
— all of which execute before the subscriber decides whether to record the span.
For high-frequency paths (thousands of calls per second), even `debug_span!`
with no active subscriber can measurably slow things down.

**Rule**: Use `info_span!` for phase-level instrumentation (a few calls per
run). For hot-path measurement, use `AtomicU64` counters or `Instant::now()`
timers aggregated at the end — orders of magnitude cheaper than spans.

### 6. Database Batching: Test Your Assumptions

Three separate batching approaches were attempted. All three regressed:

| Approach | Expected | Actual | Root Cause |
|----------|----------|--------|------------|
| Dynamic SQL batch INSERT | Faster | +22.8s | SQLite prepared statement cache beats dynamic SQL construction |
| Subquery batch DELETE | Faster | +330s | Missing index caused full table scan on subquery |
| Batch symbol lookup (IN clause) | Faster | +2.2s | Same prepared-statement advantage as INSERT |

SQLite's internal prepared statement cache makes per-row `stmt.execute(params![])`
extremely efficient — the statement is compiled once, cached, and reused. Dynamic
SQL (building `INSERT INTO ... VALUES (?,?,?), (?,?,?)...`) defeats this by
creating a new, unique statement for each batch size. The larger the batch, the
more expensive the compilation.

**Rule**: SQLite batching is not always faster. If the per-row path uses a
fixed prepared statement that SQLite can cache, it may outperform "smarter"
batching. Always measure. This is counter-intuitive for engineers coming from
PostgreSQL or MySQL where batching is almost always a win.

**A better approach at scale: defer index maintenance.** On a 14G production DB,
writing 100K rows to `references` with 4 secondary indexes online costs 21.9s.
The same data written without indexes, with indexes created afterward via
`CREATE INDEX`, costs a fraction of that. `CREATE INDEX` does a single sorted
B-tree build rather than incremental per-row insertion. The principle: **write
first, index later**. Drop all query indexes before bulk write, keep only PK
constraints, then recreate indexes and rebuild FTS in one pass at the end. TS
project: 25.81s → 18.82s (−27.1%).

### 7. Cut Negative Experiments Quickly

Of 11 optimization attempts, 5 were verified regressions and 3 were neutral.
Average time from implementation to verified rejection: ~20 minutes per attempt.

The discipline:
1. Implement the change (minimal, focused)
2. Build release
3. Run A/B on the target project
4. If regression or neutral → **revert immediately, move on**
5. If improvement → dig deeper, refine, re-verify

Don't fall in love with an idea. The levenshtein banding optimization (T0.2)
looked promising and delivered +2.6s. The batch INSERT optimization (T1.1)
looked equally promising and delivered +22.8s regression. Both passed initial
code review. Only measurement revealed which was which.

**Rule**: An optimization idea is a hypothesis, not a plan. Treat it like one:
design a falsifiable test, run it, accept the result.

### 8. Strategy Distribution Data Is More Valuable Than Timing Data

The single most important piece of profiling data from this cycle was NOT the
timing breakdown — it was the **strategy hit distribution**:

```
S1 (Builtin):      7,616 ( 2.4%)  ← dead simple, no optimization needed
S2 (Scope-local): 24,462 ( 7.8%)  ← moderate, adequate
S3 (Container):        0 ( 0.0%)  ← DEAD CODE for TypeScript
S4 (Same-file):   31,113 ( 9.9%)  ← moderate, O(1) sufficient
S5 (Import):      26,716 ( 8.5%)  ← 8.5% of calls, 53% of CPU → THE BOTTLENECK
S6 (Global/fuzzy):176,860 (56.3%) ← 56% of calls, 47% of CPU → already efficient
MISS:             47,218 (15.0%)  ← unresolvable, wasted work
```

Without this distribution, you'd naturally focus on S6 (largest call count).
But S6 is 56% of calls and 47% of CPU — each call is cheap (271μs). S5 is 8.5%
of calls and 53% of CPU — each call is expensive (2ms). The distribution data
redirected attention from call volume to per-call cost.

**Rule**: Before timing anything, count everything. A 7-counter `AtomicU64`
array costs nothing and tells you where to point the timers.

### 9. Summary Checklist

Before starting any performance optimization:

- [ ] Add lightweight counters (AtomicU64) on the hot path to measure
      distribution — what % of calls go through each code path?
- [ ] Add timing on each code path (Instant::now() → AtomicU64 aggregation)
      to measure per-call cost — which paths are expensive vs frequent?
- [ ] Establish a release-mode baseline on the largest real project available.
      Document the exact command, project state, and environment.
- [ ] Clean state before every run (delete databases, caches, incrementals).
- [ ] For each proposed optimization: make it independently revertible before
      measuring. If it regresses, revert immediately and document why.
- [ ] Trust measurements over intuition. If static analysis says "this should
      be faster" but the benchmark disagrees, the benchmark is right.

### 10. INSERT OR REPLACE Is Not Free — Even Without Conflicts

SQLite's `INSERT OR REPLACE` performs a B-tree conflict check on every row, and may do a delete-before-insert cycle — even when zero conflicts exist. On a 30K-file bulk write with pre-cleaned data, switching to plain `INSERT` cut commit_ms by 33.8% and slow chunk p95 by 45.7% (see §8 for full data and root cause analysis).

**When this applies**: Only when data has been explicitly pre-cleaned (bulk-clean-then-write, fresh index). Incremental/single-file paths must keep `INSERT OR REPLACE`.

**Implementation pattern**: `replace_on_conflict: bool` parameter — batch path sets `false`, all others `true`. Regression test verifies repeated writes with same data cause no duplication on non-batch paths.
