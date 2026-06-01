# lazy

Lazy (on-demand) dataflow engine. Builds dataflow analysis only for the functions relevant to a trace query, avoiding the cost of full-project dataflow indexing.

## Crate boundaries

- **`planner`**: reads structural index from `db`, produces a `LazyWindow` of `AnalysisUnit`s
- **`loader`**: checks unit-level `extraction_state` cache, claims an `extraction_jobs` entry on cache miss, calls `extraction` with `ExtractionMode::LazyDataflow`, and writes results to `db`
- **`constants`**: hardcoded budget caps (never exposed to MCP/CLI)

## Public API

The crate is NOT a public API — consumers use `LazyDataflowService` through the `atlas-engine` facade.

```rust
pub struct LazyDataflowService {
    store: Arc<Store>,
    project_root: Option<PathBuf>,
}

impl LazyDataflowService {
    pub fn ensure_for_position(&self, file_id, line, column) -> Result<LazyWindow>;
    pub fn ensure_for_function(&self, symbol_id) -> Result<LazyWindow>;
}
```

## Budget constants

| Constant | Value | Purpose |
|----------|-------|---------|
| `LAZY_DATAFLOW_BUDGET_MS` | 25,000 | Total wall-clock budget per lazy operation |
| `LAZY_DATAFLOW_MAX_DEPTH` | 2 | Max BFS expansion depth from seed function |
| `LAZY_DATAFLOW_MAX_UNITS` | 64 | Max AnalysisUnits in a single window |

Budget exceedance sets `LazyWindow.truncated = true` and `EnsureResult.budget_exceeded = true`, surfacing partial results with diagnostics.

## Cache policy

Each built unit is recorded in `extraction_state` with `(file_id, unit_id, layer='dataflow', content_hash, budget_exceeded)`. Cache hits skip re-extraction. Stale caches (content_hash mismatch) trigger rebuild. Concurrent misses are deduplicated through `extraction_jobs`.
