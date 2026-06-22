# analysis

Location-driven trace queries, call-graph exploration, persistent function summaries, and CFG/dataflow semantic analysis.

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
├── trace_callers(target_id, max_depth) → CallerChain
│   └── CallerPathExplorer: backward call graph walk
│
└── trace_forward(from_id, to_id, max_depth) → CallerChain
    └── ForwardPathExplorer: forward call graph walk
```

### Capability gating

Trace methods check the language's `LanguageCapabilityProfile` before execution. All 14 languages currently report DataflowFull; CFG is unsupported only for ArkTS and PHP. A supported feature can still produce a partial result when facts are missing or traversal is truncated, with the reason in diagnostics.

### Response envelope

All queries return `TraceQueryResponse<T>`:
```json
{
  "ok": true,
  "kind": "trace",
  "capability": { "language": "typescript", "capability_level": "dataflow_full" },
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

### Interprocedural summaries

`SummaryBuilder` persists parameter, return, and call-argument reachability. `CrossFunctionBridge` composes those rows into `ArgToParam` and `ReturnToCall` virtual edges during slicing. Raw `TraceEngine` does not build missing summaries; callers prepare facts through the high-level engine/filesync/lazy paths.

### Semantic CFG analysis

`EffectComposer` combines CFG and dataflow facts into language-neutral semantic effects.
Handlers compose these effects at query time onto an in-memory CFG copy; persisted CFG
nodes remain raw control-flow facts. `FieldLifecycleEngine` tracks both canonical field
paths and exact local resource variables. `BranchDiffEngine`, lifecycle proof, and semantic
impact consume the same composition. Language-specific ownership/resource meaning stays
behind analysis consumers and domain-rule registries; C/C++ defaults include common libc
and Linux kernel allocation/free APIs.
