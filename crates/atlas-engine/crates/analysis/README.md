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

Trace methods check the language's `LanguageCapabilityProfile` before execution. All 14 languages currently report DataflowInterproc and limited CFG support. C++ exposes try/catch paths through `Exception` edges. C/C++/Go/C#/PHP direct same-function goto/label pairs use persisted `Goto` edges. C# routes goto exits through intervening using `BlockExit` nodes and path-isolated finally clones from inner to outer, and rejects jumps into nested lexical/cleanup regions or out of a finally clause. PHP routes exits through path-isolated finally clones from inner to outer, rejects entry into loop/switch and either-direction finally-clause crossing, but permits entry into ordinary blocks. Lifecycle clears branch frames at a goto target because the jump may bypass or change lexical arms. JavaScript/TypeScript/ArkTS, Java, C#, PHP, Python, Kotlin, Cangjie, and Ruby additionally expose finally/ensure-style paths through path-isolated clones; Ruby includes method-body and nested begin/rescue/else/ensure, and normal and abrupt continuations cannot cross. Java/C#/PHP direct object-created explicit throws connect handlers in source order and stop at the first unguarded syntactically exact type; earlier handlers remain conservative alternatives because inheritance is unresolved. Go `select` exposes communication/default siblings without treating a blocking no-default select as a switch-style skip path. Bounded Go defer stacks are path-sensitive: normal exits traverse persisted `Defer` edges and owner-matched LIFO `BlockExit` nodes, and Deferred Free effects live on those execution nodes rather than registration points; nested call-argument consumption remains at registration. Rust `?` preserves both its success continuation and residual return-to-Exit path while respecting nested closure/async boundaries. Rust `let-else` separates successful matching from explicit return/break/continue, unconditional-loop, or unqualified builtin panic-like macro alternatives. Macro shadowing/re-exports, custom never-return macros, panic unwinding, and `catch_unwind` remain conservative. Rust implicit Drop remains a function-exit effect heuristic, not path-sensitive lexical RAII. Python unguarded syntax-irrefutable wildcard/capture/`as`/group/OR arms suppress the impossible synthetic no-match path; Rust and Cangjie currently recognize only direct unguarded wildcards. Guarded and type-driven pattern exhaustiveness stays conservative. Java try-with-resources, C# using, Python with, Kotlin use, and Ruby block resources use persisted lexical owners to bind each allocation only to that scope's path-isolated BlockExit clones, with deterministic LIFO cleanup effects. Cleanup exceptions conservatively retain ordered `Throw` continuations into enclosing handlers/finally regions. Cyclic or over-budget Go defer stacks fall back atomically to annotation; panic/recover/Goexit unwinding, complex anonymous deferred bodies, inherited or aliased catch types, thrown variables, guarded/filtered handlers, implicit exceptions, remaining complex match exhaustiveness and pattern binding dataflow, cleanup exception suppression/replacement and exact identity, Ruby retry/redo, computed goto, C# goto case/default, C++ cross-scope destruction on goto, ArkUI callback/trailing-block control flow, and the bounded clone fallback remain explicit boundaries. A supported feature can still produce a partial result when facts are missing or traversal is truncated, with the reason in diagnostics.

### Response envelope

All queries return `TraceQueryResponse<T>`:
```json
{
  "ok": true,
  "kind": "trace",
  "capability": { "language": "typescript", "capability_level": "dataflow_interproc" },
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
paths and exact local resource variables. Lifecycle transitions carry owner-bound branch
frames for true/false/case paths and for exception handlers; entering a handler discards
frames owned by the abandoned try region, preserves enclosing conditions, and the frame is
removed at that try's Join. MCP exposes this as each transition's `branch_context`.
`BranchDiffEngine`, lifecycle proof, and semantic impact consume the same composition;
branch diff deliberately does not compare handlers as ordinary true/false siblings.
Language-specific ownership/resource meaning stays behind analysis consumers and
domain-rule registries; C/C++ defaults include common libc and Linux kernel allocation/free
APIs.
