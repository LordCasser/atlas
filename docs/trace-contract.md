# Atlas Trace Contract — v1

> **Status**: Frozen for agent consumption. Types documented here are the stable
> public API for CLI `--json` output and MCP tool responses. Internal
> implementation may change; the contract (type names, field names, semantics)
> is version-locked.
>
> Important: this contract describes provenance tracing and caller-path querying.
> It is not a vulnerability scanner contract and does not define vulnerability
> rules, findings, or automatic project-wide security propagation.

## Architecture

```
User Query
     │
     └─ MCP:  tools/call { "name": "trace", "arguments": { "kind": "point", ... } }
              │
              ▼
         TraceEngine
              │
              ├─ trace_point()    → TracePoint
              ├─ trace_variable() → VariableTracePath (= TracePath)
              └─ trace_callers()  → CallerPath (= CallerChain)
              │
              ▼
         TraceQueryResponse<T>
              │
              ▼
         JSON → AI Agent / CLI user
```

All entry points return **the same envelope** — `TraceQueryResponse<T>` —
so consumers parse one shape regardless of which query was made.

---

## 1. TraceQueryResponse<T> — outer envelope

```json
{
  "ok": true,
  "kind": "trace_variable",
  "capability": { "language": "TypeScript", "capability_level": "dataflow_full", ... },
  "partial_result": false,
  "diagnostics": [],
  "result": { ... }
}
```

| Field | Type | Always present? | Semantics |
|-------|------|:---:|-----------|
| `ok` | `bool` | ✅ | Transport-level success. `false` only on system errors (I/O, DB corruption). |
| `kind` | `string` | ✅ | One of `"trace"` (unified; sub-kind in result). |
| `capability` | `LanguageCapabilityProfile\|null` | ✅ | The resolved language's capability profile. `null` when `ok=false`. |
| `partial_result` | `bool` | ✅ | `true` when result is incomplete. Inspect `diagnostics`. |
| `diagnostics` | `TraceDiagnostic[]` | ✅ | May be empty. Each entry has `level`, `message`, optional `code`. |
| `result` | `T\|null` | ✅ | The query result. `null` only when `ok=false`. May be `Some(T)` even when `partial_result=true` (result is present but was truncated). |

### Contract invariants

1. **All 6 fields are always present** in JSON — never omitted via `skip_serializing_if`.
2. `ok=true` + `partial_result=true` → **not an error**; query was processed but result may be incomplete due to truncation or capability limits.
   Inspect `diagnostics` for structured reason codes.
3. `ok=false` → **system error**; only possible result is `diagnostics[0].level = "error"`.
4. `capability` is present even in partial/error cases (may be `null` for errors).

MCP wrappers may add top-level fields such as `query_id`, `precision`, `lazy_diagnostics`, and `analysis_contract`. If a trace request triggered lazy structural or lazy dataflow, `lazy_diagnostics` and `analysis_contract` must be present even when `result` is empty or no path is found.

---

## 2. TracePoint — trace_point result

```json
{
  "reference": { "id": "...", "text": "helper", "kind": "call", "range": {...}, "resolved": {...} },
  "resolved_symbol": { "id": "...", "name": "helper", "kind": "Function", ... },
  "data_node": { "id": "...", "name": "result", "kind": "local", ... },
  "incoming": [{ "node_id": "...", "name": "base", "kind": "local", ... }],
  "outgoing": [],
  "binding": { "id": "...", "name": "result", "kind": "let", ... },
  "binding_use": null,
  "scope": { "id": "...", "name": "compute", "kind": "Function", ... },
  "callsite": { "id": "...", "callee_range": {...}, "range": {...}, ... },
  "file_id": "...",
  "line": 4, "column": 18,
  "capability": {...},
  "partial_result": false,
  "diagnostics": []
}
```

| Field | Source | Semantics |
|-------|--------|-----------|
| `reference` | extract step 9 | The reference (import/call/use) containing the query position. |
| `resolved_symbol` | ReferenceResolver | The definition the reference resolves to. |
| `data_node` | DataFlowBuilder | The data node whose range contains the position. |
| `incoming` | dataflow edges | Data nodes that flow INTO this point (predecessors). |
| `outgoing` | dataflow edges | Data nodes that flow OUT of this point (successors). |
| `binding` | extract bindings | The lexical binding definition at this position. |
| `binding_use` | extract binding_uses | A reference to a binding at this position. |
| `scope` | extract scopes | The enclosing scope (function, class, block). |
| `callsite` | extract callsites | If at a call expression, full callsite with callee range. |
| `file_id`, `line`, `column` | query params | Echo of the user's query position. |

---

## 3. VariableTracePath (= TracePath) — trace_variable result

```json
{
  "source": { <TracePoint> },
  "steps": [ <TracePathStep>, ... ],
  "sink": { <TracePoint> },
  "confidence": 0.85,
  "nodes_visited": 12,
  "capability": {...},
  "partial_result": false,
  "diagnostics": []
}
```

- `source` — the farthest origin point the slicer reached.
- `sink` — the user-chosen query position.
- `steps[]` — ordered from origin → query point.

### TracePathStep

```json
{
  "index": 0,
  "from_node_id": "...",
  "to_node_id": "...",
  "edge_kind": "assign",
  "description": "x assigned to y",
  "file_id": "...",
  "range": { "start_line": 4, ... }
}
```

---

## 4. CallerChain — trace_caller_path result

```json
{
  "root": { "id": "...", "name": "main", "kind": "Function", ... },
  "steps": [ <CallerChainStep>, ... ],
  "target": { "id": "...", "name": "helper", "kind": "Function", ... },
  "nodes_visited": 42,
  "max_depth_reached": 3
}
```

- `root` — the farthest caller found.
- `target` — the function the user queried.
- `steps[]` — ordered root → target.

### CallerChainStep

```json
{
  "index": 0,
  "caller": "<SymbolId hex>",
  "callee": "<SymbolId hex>",
  "edge_kind": "calls",
  "callsite": { "id": "...", "range": { ... }, "callee_range": { ... } },
  "file_id": "...",
  "range": { "start_line": 6, "start_column": 5, "end_line": 6, "end_column": 13 },
  "description": "main → helper"
}
```

### Single-chain semantics (locked)

The explorer returns the **single farthest** caller chain via BFS. It does NOT enumerate all possible paths.

---

## 4b. ForwardCallChain — trace_forward result

Shares the same shape as `CallerChain` but the BFS walks **forward** (caller → callee) rather than backward.

```json
{
  "root": { "id": "...", "name": "main", "kind": "Function", ... },
  "steps": [ <CallerChainStep>, ... ],
  "target": { "id": "...", "name": "processRequest", "kind": "Function", ... },
  "nodes_visited": 84,
  "max_depth_reached": 5
}
```

- `root` — the source function the user queried.
- `target` — the destination function reached.
- `steps[]` — ordered root → target (same `CallerChainStep` type as caller_path).
- `kind` in envelope: `"trace_forward"`.

---

## 5. Evidence

```json
{
  "file_path": "src/app.ts",
  "snippet": "    const msg = greet(\"World\");",
  "symbol_name": "greet"
}
```

---

## 6. TraceDiagnostic

```json
{
  "level": "warning",
  "message": "Dataflow not supported for this language",
  "code": "unsupported_language"
}
```

| `level` | `code` examples | Semantics |
|---------|----------------|-----------|
| `"info"` | — | Pure informational note. |
| `"warning"` | `no_data_node`, `no_trace_path`, `no_callers` | Query processed but no result. |
| `"warning"` | `unsupported_language` | Capability gate: language lacks dataflow or call_graph. |
| `"error"` | (any) | System failure; `ok=false`. Only here on genuine errors. |

---

## 7. Capability Model

Each language has a `LanguageCapabilityProfile` with `FeatureMatrix` for fine-grained per-feature capability checks. All 14 languages are at `DataflowFull` level.

### Capability gating

- `trace(kind="variable")`: gated on `local_dataflow.is_supported()`.
- `trace(kind="callers")`: gated on `call_graph.is_supported()`.
- `trace(kind="forward")`: gated on `call_graph.is_supported()`.
- `trace(kind="point")`: **always available**, regardless of capability.

Capability gating is not a substitute for lazy diagnostics. A supported query can still return partial/no-result because lazy extraction was budget-limited, pending, or unable to build the needed facts; those cases must be explained through `diagnostics` plus MCP `lazy_diagnostics`.

---

## 8. MCP Tool Contracts

A single unified `trace` tool replaces the four pre-v1.3.1 tools
(`trace_point`, `trace_variable`, `trace_caller_path`, `trace_forward`).
The `kind` parameter selects the trace mode.

| Kind | Purpose |
|------|---------|
| `"point"` | Resolve a code position to full context |
| `"variable"` | Walk backward through dataflow edges to find value origins |
| `"callers"` | Walk backward through call edges to find caller chain |
| `"forward"` | Walk forward through call edges (how does A reach B?) |

### tools/list returns:

```json
{
  "name": "trace",
  "description": "Source-level trace queries...",
  "inputSchema": {
    "type": "object",
    "properties": {
      "kind": {
        "type": "string",
        "enum": ["point", "variable", "forward", "callers"],
        "description": "Trace kind: 'point' for source position resolution, 'variable' for backward dataflow, 'forward' for forward call chain, 'callers' for backward call chain."
      },
      "file_path": { "type": "string", "description": "File path relative to project root (e.g. 'src/foo.ts')." },
      "file_id": { "type": "string", "description": "File ID in hex (alternative to file_path)." },
      "line": { "type": "integer", "description": "1-based line number." },
      "column": { "type": "integer", "description": "1-based column number." },
      "symbol": { "description": "Qualified symbol name or SymbolSelector object (required for kind='callers'). See Symbol Selector in tool documentation for the structured format." },
      "from": { "description": "Source qualified symbol name or SymbolSelector object (required for kind='forward')." },
      "to": { "description": "Target qualified symbol name or SymbolSelector object (required for kind='forward')." },
      "max_depth": { "type": "integer", "description": "Maximum traversal depth (default varies by kind)." },
      "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots." }
    },
    "required": ["kind"]
  }
}
```

### The `trace` tool returns `CallToolResult`:

```json
{
  "content": [{
    "type": "text",
    "text": "<serialized TraceQueryResponse<T>>"
  }],
  "isError": false
}
```

- `isError` is derived from `TraceQueryResponse.ok`: `isError = !ok`.

---

## 9. What Is NOT in This Contract

- ❌ **Taint analysis** — Atlas does not include taint analysis.
- ❌ **Multi-path caller chain** — single farthest chain only.
- ❌ **Full compiler-grade type checking**.
- ❌ **CFG-based feasibility** — dataflow slicing is use-def chain based.
- ❌ **Indexing semantics** — incremental indexing/sync is not part of the trace JSON contract.

---

## 10. Usage Examples

> **Note:** Trace functionality is only available via MCP tools. There is no `atlas trace` CLI command.

### MCP (from AI agent)

```json
// Request
{ "method": "tools/call", "params": {
    "name": "trace",
    "arguments": { "kind": "variable", "file_path": "src/app.ts", "line": 4, "column": 18, "max_depth": 20 }
}}

// Response — always parse the envelope first, then check result
{ "content": [{"type": "text", "text": "{\"ok\":true,\"kind\":\"trace\",...}"}],
  "isError": false }
```

### Parsing pattern

```
1. Parse JSON
2. If !ok: read diagnostics, handle error
3. If partial_result: read diagnostics, treat result as best-effort
4. Otherwise: result is complete
```
