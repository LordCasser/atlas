# atlas-cli

CLI binary for Atlas. Command dispatch, logging, and integration tests.

## Commands

| Command | Purpose |
|---------|---------|
| `atlas` (no subcommand) | Launch interactive TUI (search, symbol detail, caller trace) |
| `atlas index` | Auto-initialize `.atlas/` schema and index source files |
| `atlas sync` | Incrementally update index after file changes |
| `atlas status` | Show index statistics and project health |
| `atlas doctor` | Check schema, SQLite, grammar, capability readiness |
| `atlas files` | List indexed files with language and parse status |
| `atlas mcp` | Start MCP server (requires `mcp` feature) |

## Build features

```bash
# Default: TypeScript, JavaScript, Python
cargo build -p atlas-cli

# All languages + MCP
cargo build -p atlas-cli --features "all-languages,mcp"
```

## Test structure

- `tests/trace_mcp_e2e.rs` — MCP tool dispatch end-to-end tests
- `tests/trace_cli_e2e.rs` — CLI trace command tests
- `tests/trace_e2e.rs` — Lower-level trace engine tests
- `tests/golden.rs` — Golden file snapshot tests
- `tests/integration.rs` — Full pipeline integration tests
