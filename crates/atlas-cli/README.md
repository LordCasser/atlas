# atlas-cli

CLI binary for Atlas. Command dispatch, logging, and integration tests.

## Commands

| Command | Purpose |
|---------|---------|
| `atlas` (no subcommand) | Launch the interactive code-graph workbench; recover/create the DB and run default structural indexing first when no usable basic index exists |
| `atlas index` | Auto-initialize `.atlas/` schema and index source files |
| `atlas sync` | Incrementally update index after file changes |
| `atlas status` | Show index statistics and project health |
| `atlas doctor` | Check schema, SQLite, grammar, capability readiness |
| `atlas files` | List indexed files with language and parse status |
| `atlas mcp` | Start MCP server (requires `mcp` feature) |

## Build features

```bash
# All 14 languages are compiled by default
cargo build -p atlas-cli

# With MCP server
cargo build -p atlas-cli --features mcp
```

## Interactive TUI

The bare `atlas` command opens the Ratatui workbench. Symbol search, the
overview/callers/callees/peers/source views, and caller tracing are native
high-frequency views. Press `:` to open the command palette for the shared MCP
analysis handlers: `symbol`, `calls`, `explore`, `impact`, `path`, `trace`,
`file_dependencies`, `lifecycle`, `branch_diff`, `domain_rules`, and
`fp_dispatches`, plus `tasks` and `resume_query` for non-terminal focus work.

The selected symbol and file are injected automatically. Selecting a command
opens a field form: required values are marked with `*`, choices and booleans
cycle with Left/Right, and text or numeric fields are edited with Enter.
Variant-dependent forms show only fields used by the current `trace` kind or
management action. Validation and submitted arguments use that same field rule,
so hidden parameters cannot leak into calls and users do not construct MCP JSON.

`Enter` opens the highlighted command, `Tab` moves between active form fields,
and `Esc` returns to the previous layer. Analysis output uses the full workbench
body and defaults to a human-oriented projection: code facts, source, paths,
dependencies, rules, and diagnostics are rendered as structured sections;
analysis state, capability, confidence, coverage, and truncation are summarized
in an adaptive HUD. Arrows, `j`/`k`, and Page Up/Page Down scroll. `r` toggles
the untouched raw response for auditing; `x` or `Esc` returns to the workbench.

The palette calls one session-persistent `atlas_mcp::tools::ToolRouter` in the
existing TUI worker, so it uses the same handler, lazy-analysis behavior,
response envelope, and error semantics as MCP. The latest response `query_id`
is retained and prefilled for `tasks` and `resume_query`, while remaining
editable. Project lifecycle and search are intentionally not palette commands:
the TUI owns one local project and provides native symbol search.

The result projector is presentation-only. It never invents precision or
coverage, and it preserves unknown non-metadata fields in the facts view rather
than silently discarding future handler output. Root control metadata is moved
to the HUD or diagnostics; the raw response remains the authoritative fallback.

## Test structure

- `tests/trace_e2e.rs` — End-to-end trace semantics over extracted and persisted facts
- `tests/trace_fixtures.rs` — Per-language provenance and cross-function bridge fixtures
- `tests/lazy_index_e2e.rs` — Manifest, structural, lazy dataflow, and capability-state behavior
- `tests/golden.rs` — Golden file snapshot tests
- `tests/integration.rs` — Full pipeline integration tests
- `tests/deep_nesting_test.rs` — Parser/extraction stack-safety regressions
