# analysis

Location-driven trace queries, call-graph exploration, and function summaries.

## Components

### TraceEngine (`trace/engine.rs`)

Unified public API for all trace queries. Wraps Locator, Slicer, and CallerPathExplorer.

```rust
TraceEngine
├── trace_point(file_id, line, column) → TracePoint
│   └── Locator: resolves position to { symbol, data_node, scope, bindings, ... }
│
├── trace_variable(file_id, line, column, max_depth) → TracePath
│   └── Slicer: backward dataflow walk (Assign → FieldLoad → ArgToParam → UseDef)
│
└── trace_callers(target_id, max_depth) → CallerChain
    └── CallerPathExplorer: backward call graph walk
```

### Capability gating

`trace_variable` and `trace_callers` check the language's `LanguageCapabilityProfile` before execution. Unsupported languages return `partial_result = true` with diagnostic info, never errors.

### Response envelope

All queries return `TraceQueryResponse<T>`:
```json
{
  "ok": true,
  "kind": "trace_variable",
  "capability": { "language": "typescript", "level": "DataflowBasic" },
  "partial_result": false,
  "diagnostics": [],
  "result": { ... }
}
```

### Locator (`trace/locator.rs`)

Maps `(file_id, line, column)` → `TracePoint`:
1. Find innermost reference at position
2. Find resolved symbol
3. Find enclosing scope
4. Find data node (if dataflow exists)
5. Find bindings and binding uses
6. Collect incident dataflow edges

### Slicer (`trace/slicer.rs`)

Backward dataflow walk from a `DataNodeId`:
1. Start at the node at cursor position
2. Follow incoming dataflow edges: `Assign`, `FieldLoad`, `ArgToParam`, `Read`, `UseDef`
3. Build `TracePath` steps with source snippets (if `project_root` available)
4. Stop at parameters, literals, globals, or `max_depth`
