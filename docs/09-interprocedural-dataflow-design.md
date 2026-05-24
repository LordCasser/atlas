# Inter-procedural Dataflow Summary Layer — Design Notes

## Current State

Local (intra-procedural) dataflow is working for all DataflowBasic languages:
- DataNodes for parameters, locals, fields, call args, returns
- DataFlowEdges for Assign, FieldLoad, FieldStore, ArgToCall, Read, ReturnValue
- use-def resolution groups nodes by (function_id, binding_id)

Inter-procedural flow is currently handled at trace-time via `SummaryEdgeProvider`:
- `ArgToParam`: bridges caller CallArg → callee Parameter (virtual edge)
- `ReturnToCall`: bridges callee return → caller call-result Expr (virtual edge)
- These edges are NOT persisted in `dataflow_edges` table
- They are materialized on-demand during `TraceEngine::trace_variable`
- All language capability profiles declare `interprocedural_summaries: unsupported`

## Design Goals

1. **Persistent summaries**: store enough fact data to reconstruct inter-proc flow without re-running extraction
2. **Scalable**: avoid O(N×M) edge explosion for large codebases (summary per function, not per call-site)
3. **Language-agnostic**: same schema for all languages; language-specific logic only in extraction
4. **Progressive precision**: start with basic name-based bridging, refine to type/CFG/alias-sensitive later

## Proposed Schema (three new DB tables)

### `function_summaries`
```sql
CREATE TABLE function_summaries (
    function_id  TEXT NOT NULL,        -- SymbolId of the function
    param_count  INTEGER NOT NULL,
    return_count INTEGER NOT NULL DEFAULT 1,
    summary_hash TEXT NOT NULL,        -- blake3 of the summary facts (for cache invalidation)
    PRIMARY KEY (function_id),
    FOREIGN KEY (function_id) REFERENCES symbols(id)
);
```

### `summary_param_sources`
```sql
-- Which data nodes flow into each parameter (backward slice from param)
CREATE TABLE summary_param_sources (
    function_id  TEXT NOT NULL,
    param_index  INTEGER NOT NULL,     -- 0-based parameter position
    source_type  TEXT NOT NULL,        -- 'direct_param', 'field_of_param', 'call_result', 'literal'
    source_data_node_id TEXT,          -- the DataNodeId that reaches this param
    confidence   REAL NOT NULL DEFAULT 0.5,
    provenance   TEXT,                 -- human-readable explanation
    PRIMARY KEY (function_id, param_index, source_data_node_id),
    FOREIGN KEY (function_id) REFERENCES function_summaries(function_id)
);
```

### `summary_return_sinks`
```sql
-- Which data nodes flow into each return (backward slice from return)
CREATE TABLE summary_return_sinks (
    function_id  TEXT NOT NULL,
    return_index INTEGER NOT NULL DEFAULT 0,
    source_data_node_id TEXT NOT NULL, -- DataNodeId that reaches the return
    edge_kind    TEXT NOT NULL,        -- 'Assign', 'FieldLoad', 'CallResult', etc.
    confidence   REAL NOT NULL DEFAULT 0.5,
    provenance   TEXT,
    PRIMARY KEY (function_id, return_index, source_data_node_id),
    FOREIGN KEY (function_id) REFERENCES function_summaries(function_id)
);
```

## Trace-time Materialization

When `TraceEngine` encounters a `CallArg` → `CallTarget` edge, instead of computing virtual edges on-the-fly:

1. Look up `function_summaries` for the callee
2. If summary exists:
   - For each `summary_param_sources`: create `ArgToParam(source=call_arg_data_node, target=callee_param_data_node)`
   - For each `summary_return_sinks`: create `ReturnToCall(source=callee_return_data_node, target=caller_expr_data_node)`
3. If summary does NOT exist:
   - Fall back to current virtual edge logic (`SummaryEdgeProvider`)
   - Flag as `low_confidence` + `provenance = "on-demand (no cached summary)"`

## Migration Path

### Phase 1: Compute + Cache (no schema change)
- Keep current virtual edge logic
- Cache computed summaries in memory (HashMap<SymbolId, FunctionSummary>)
- Add `FunctionSummaryCache` to `TraceEngine`
- No persistent storage yet

### Phase 2: Persist summaries
- Add the three tables above
- `DataFlowBuilder` (or a new `SummaryBuilder`) computes summaries per-function after extraction
- Store in DB during `insert_file_facts`
- `TraceEngine` reads from DB instead of computing on-the-fly

### Phase 3: Incremental update
- Recompute summary when function body changes (content_hash diff)
- Invalidate downstream callers (transitive invalidation via call graph)
- Batch recompute for dirty functions only

## Language Readiness

| Language | Local Dataflow | Call Graph | Ready for Summary? |
|----------|---------------|------------|-------------------|
| TypeScript | ✅ | ✅ | ✅ Yes |
| JavaScript | ✅ | ✅ | ✅ Yes |
| Python | ✅ | ✅ | ✅ Yes |
| Java | ✅ | ✅ | ✅ Yes |
| Go | ✅ | ✅ | ✅ Yes |
| C# | ✅ | ✅ | ⚠️ TraceEngine path empty |
| Rust | ✅ | ✅ | ✅ Yes |
| PHP | ✅ | ✅ | ⚠️ TraceEngine path empty |
| Kotlin | ✅ | ✅ | ⚠️ TraceEngine path empty |
| Ruby | ✅ | ✅ | ✅ Yes |
| C | ✅ | ✅ | ⚠️ Best-effort |
| C++ | ✅ | ✅ | ⚠️ Best-effort |
| ArkTS | ✅ | ✅ | ⚠️ TS delegate |
| Bash | ❌ | ❌ | ❌ No |
| Cangjie | ❌ | ❌ | ❌ No |

## Recommendation

Start with Phase 1 (in-memory cache) for TypeScript and Python first.
These two have the strongest local dataflow + call graph + TraceEngine path support.
Once the cache proves correct, move to Phase 2 persistence for all ready languages.
