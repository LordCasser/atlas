# focus_materialize

Focus **internal** on-demand dataflow materialize (planner + loader + budgets).

This crate is **not** a product entry point. Query-time product semantics are
**Focus** (`FocusRuntime` + `FocusMaterialize` in `atlas-engine`). Obtain a
configured dataflow service only via `FocusMaterialize::open`.

## Modules

- **`planner`**: reads structural index from `db`, produces a `LazyWindow` of `AnalysisUnit`s
- **`loader`**: checks unit-level `extraction_state` cache, claims `extraction_jobs`,
  runs `ExtractionMode::LazyDataflow`, writes results to `db`
- **`constants`**: hardcoded budget caps (never exposed to MCP/CLI)

## Names and construction

- Mechanism types: `LazyDataflowService`, `LazyWindow` (not AccessStrategy / product path).
- Production entry: `atlas_engine::FocusMaterialize::open`.
- Cross-crate factory: `with_structural_rebuilder` (`#[doc(hidden)]`, rebuilder required).
- Unit write: invalid `data_node.binding_id` → SET NULL; invalid `function_id` → drop row.
