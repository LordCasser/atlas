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

## Mechanism type names

`LazyDataflowService` / `LazyWindow` remain as **mechanism** type names (L2
extraction / window IR). They are not an AccessStrategy or parallel product line.

## Construction

- Production: only via `atlas_engine::FocusMaterialize::open`.
- Factory: `LazyDataflowService::with_structural_rebuilder` is `#[doc(hidden)]`
  and always requires a rebuilder.
- Unit write FK: invalid `data_node.binding_id` is cleared (SET NULL), not dropped
  with the whole node — see `db::fk_guards::filter_data_nodes`.
