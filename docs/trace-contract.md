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
    ├─ CLI:  atlas trace point/variable/caller-path --json
    │
    └─ MCP:  tools/call { "name": "atlas_trace_point", ... }
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

All three entry points return **the same envelope** — `TraceQueryResponse<T>` —
so consumers parse one shape regardless of which query was made.

---

## 1. TraceQueryResponse<T> — outer envelope

```json
{
  "ok": true,
  "kind": "trace_variable",
  "capability": { "language": "TypeScript", "capability_level": "DataflowBasic", ... },
  "partial_result": false,
  "diagnostics": [],
  "result": { ... }
}
```

| Field | Type | Always present? | Semantics |
|-------|------|:---:|-----------|
| `ok` | `bool` | ✅ | Transport-level success. `false` only on system errors (I/O, DB corruption). |
| `kind` | `string` | ✅ | One of `"trace_point"`, `"trace_variable"`, `"trace_callers"`. |
| `capability` | `LanguageCapabilityProfile\|null` | ✅ | The resolved language's capability profile. `null` when `ok=false`. |
| `partial_result` | `bool` | ✅ | `true` when result is incomplete (unsupported lang, no data node, no callers). Inspect `diagnostics`. |
| `diagnostics` | `TraceDiagnostic[]` | ✅ | May be empty. Each entry has `level`, `message`, optional `code`. |
| `result` | `T\|null` | ✅ | The query result. `null` when `partial_result=true` or `ok=false`. |

### Contract invariants

1. **All 6 fields are always present** in JSON — never omitted via `skip_serializing_if`.
2. `ok=true` + `partial_result=true` → **not an error**; query was processed but no result.
   Inspect `diagnostics` for structured reason codes.
3. `ok=false` → **system error**; only possible result is `diagnostics[0].level = "error"`.
4. `capability` is present even in partial/error cases (may be `null` for errors).

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

### What to expect

- **At a call position**: `reference` + `resolved_symbol` + `callsite` (P0).
- **At a variable use**: `reference` + `binding_use` + `data_node`.
- **At a variable definition**: `binding` + `data_node`.
- **At blank/comment space**: All fields likely `null/[]`, still valid JSON.

---

## 3. VariableTracePath (= TracePath) — trace_variable result

`source` and `sink` are legacy v1 field names in the current JSON shape. Their
semantics are provenance-only:

- `source` means the farthest origin point reached by the backward slicer.
- `sink` means the user-chosen query point.

They do not refer to vulnerability rule concepts and must not be interpreted as
scanner input/output categories.

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
- `sink` — the user-chosen query position (same as trace_point at that position).
- `steps[]` — ordered from origin → query point, each step is one dataflow edge.

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

## 4. CallerPath (= CallerChain) — trace_callers result

```json
{
  "root": { "id": "...", "name": "main", "kind": "Function", ... },
  "steps": [ <CallerChainStep>, ... ],
  "target": { "id": "...", "name": "helper", "kind": "Function", ... },
  "nodes_visited": 42,
  "max_depth_reached": 3
}
```

- `root` — the farthest caller found (entry-point or exported function).
- `target` — the function the user queried.
- `steps[]` — ordered root → target, each step is one call edge.

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

**Key evidence**: `range` points to the **actual call expression** (e.g., `helper(21)`),
not the caller function's definition. `callsite.reference_id` can be traced back
to the original call reference via Store queries.

### Single-chain semantics (locked)

The explorer returns the **single farthest** caller chain via BFS from target to
root. It does NOT enumerate all possible paths. This is an intentional design
choice for bounded output. Future multi-path support will be opt-in via a new
tool or parameter.

---

## 5. Evidence

```json
{
  "file_path": "src/app.ts",
  "snippet": "    const msg = greet(\"World\");",
  "symbol_name": "greet"
}
```

Attached to `TraceQueryResponse` or individual trace steps to provide human-readable
context without extra database queries. Consumer-readable fields only; no internal IDs.

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

Each language has a `LanguageCapabilityProfile` that includes:

1. **Coarse capability level** (`CapabilityLevel`): `None`, `Symbolic`, `DataflowBasic`, `DataflowFull`
2. **Feature matrix** (`FeatureMatrix`): fine-grained per-feature flags with confidence floor, limitations, and structured reasons for unsupported features

### Per-language FeatureMatrix (key features)

| Feature | TS/JS | Python | Java | C/C++ | ArkTS | Go/Rust/C#/PHP/Ruby/Kotlin | Bash opt-in | Cangjie opt-in |
|---------|:-----:|:------:|:----:|:-----:|:-----:|:-------------------------:|:-----------:|:-------:|
| symbols | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓††† | ✓ |
| references | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓††† | ✓ |
| call_graph | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | ✓††† | ✗ |
| lexical_bindings | ✓† | ✓†† | ✓†† | ✓†† | ✓†† | ✓†† | ✗ | ✗ |
| local_dataflow | ✓† | ✓† | ✓† | ✓† | ✓† | ✓† | ✗ | ✗ |
| use_def | ✓†† | ✓†† | ✓†† | ✓†† | ✓†† | ✓†† | ✗ | ✗ |
| cfg | ✓ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |
| interprocedural | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ | ✗ |

- ✓ = `Supported { confidence_floor: profile-specific }`
- ✓† = Supported with limitations; local dataflow is AST-driven but still has language-specific gaps
- ✓†† = Supported with reduced precision, primarily name/scope heuristic binding
- ✓††† = Symbolic/low-confidence extraction only; trace variable provenance remains unsupported
- ✗ = `Unsupported { reason: "..." }` with structured diagnostic

### Capability gating

Gating uses the **FeatureMatrix** (not the coarse `CapabilityLevel`):

- `trace_variable`: gated on `FeatureMatrix.local_dataflow.is_supported()`.
  Falls back to coarse `capability_level >= DataflowBasic` for backward compat.
  If unsupported, response is `ok=true, partial_result=true`
  with `diagnostics[].code = "unsupported_language"` and the
  `Unsupported { reason }` string in the diagnostic message.

- `trace_callers`: gated on `FeatureMatrix.call_graph.is_supported()`.
  Falls back to `"call_graph" in supported_features` for backward compat.
  If unsupported, same partial response pattern.

- `trace_point`: **always available**, regardless of capability.

---

## 8. MCP Tool Contracts

### tools/list returns:

```json
{
  "name": "atlas_trace_point",
  "description": "Resolve a code position...",
  "inputSchema": {
    "type": "object",
    "properties": {
      "file_id": { "type": "string", "description": "File ID hex from atlas_files" },
      "file_path": { "type": "string", "description": "Relative file path" },
      "line": { "type": "integer", "minimum": 1 },
      "column": { "type": "integer", "minimum": 1 }
    },
    "required": []
  }
}
```

Same pattern for `atlas_trace_variable` (+ `max_depth`) and
`atlas_trace_caller_path` (takes `symbol` hex ID or `symbol_name` instead of `file_id`/`file_path`/`line`/`column`). The current MCP schema permits either file identity form and validates missing arguments inside the handler so errors can use the same `TraceQueryResponse` envelope.

### All three return `CallToolResult`:

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

These are explicitly excluded from the current frozen trace contract:

- ❌ **Taint analysis** — Atlas does not include taint analysis. No scanner rules, finding tables, or scanner engine.
- ❌ **Multi-path caller chain** — single farthest chain only.
- ❌ **Full interprocedural dataflow** — slicing is primarily intra-procedural,
  with limited call-argument/caller-path evidence where facts exist.
- ❌ **CFG-based feasibility** — dataflow slicing is use-def chain based,
  not control-flow-graph reachability.
- ❌ **Multi-language union types** — each file assumes a single language.
- ❌ **Indexing semantics** — incremental indexing/sync is an indexing concern,
  not part of the trace JSON contract.

---

## 10. Usage Examples

### CLI

```bash
# Get full trace response as JSON
atlas trace point --file src/app.ts --line 10 --column 15 --json
atlas trace variable --file src/app.ts --line 10 --column 15 --json
atlas trace caller-path --symbol <symbol-hex> --json

# Human-readable
atlas trace point --file src/app.ts --line 10 --column 15
```

### MCP (from AI agent)

```json
// Request
{ "method": "tools/call", "params": {
    "name": "atlas_trace_variable",
    "arguments": { "file_path": "src/app.ts", "line": 4, "column": 18, "max_depth": 20 }
}}

// Response — always parse the envelope first, then check result
{ "content": [{"type": "text", "text": "{\"ok\":true,\"kind\":\"trace_variable\",...}"}],
  "isError": false }
```

### Parsing pattern

```
1. Parse JSON
2. If !ok: read diagnostics, handle error
3. If partial_result: read diagnostics, treat result as best-effort
4. Otherwise: result is complete
```
