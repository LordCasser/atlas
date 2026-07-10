//! Tool schema definitions: constructs the 15-tool MCP surface.
//!
//! Pure free functions that build [`Tool`] input-schema definitions.
//! Moved out of `mod.rs` for readability (DEBT-3); no logic changes.

use super::*;

// ===================================================================
// Tool registration — 15 tools (open-first focus MCP surface)
// ===================================================================

// ── Project tools ────────────────────────────────────────────────────

fn make_project_tools() -> Vec<Tool> {
    vec![Tool {
        name: "project".into(),
        description: "Open, inspect, or list files in a project. Use action='open' to synchronously activate a project backed by project/.atlas/atlas.db; MCP open never indexes or scans the whole tree. Explicit indexing is CLI-only (`atlas index`). action='status' reports the active project and focus state; action='files' lists known project files.".into(),
        input_schema: ToolInputSchema {
            schema_type: "object".into(),
            properties: Some(json!({
                "action": {
                    "type": "string",
                    "enum": ["open", "status", "files"],
                    "description": "Operation: 'open' activates a project, 'status' shows overview, 'files' lists known project files."
                },
                "project_path": { "type": "string", "description": "Absolute path to the project directory to open (required for action='open')." },
                "verbose": { "type": "boolean", "description": "Include verbose details (action='status')." },
                "limit": { "type": "integer", "description": "Max files returned (action='files', default unlimited)." },
                "language": { "type": "string", "description": "Filter files by language (action='files', e.g. 'rust', 'typescript')." },
                "path_prefix": { "type": "string", "description": "Filter files by path prefix (action='files')." },
            })),
            required: None,
        },
    }]
}
// ── SymbolSelector schema helpers ────────────────────────────────────

fn symbol_selector_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "description": "Structured symbol selector with fault-tolerant scoring. Fields outside qualified_name are used for ranking only — incorrect values cannot prevent the correct symbol from being found.",
        "required": ["qualified_name"],
        "properties": {
            "qualified_name": {
                "type": "string",
                "description": "Qualified symbol name. REQUIRED. The highest-priority signal. If this uniquely identifies a symbol, other fields are ignored (but actual values are always returned in the response)."
            },
            "file_path": {
                "type": "string",
                "description": "Project-relative file path (e.g. 'src/foo.ts'). Supports suffix, basename, and fuzzy matching — no need to be exact. Used for ranking when qualified_name matches multiple symbols."
            },
            "line": {
                "type": "integer",
                "description": "1-based line number. Used for ranking within the same file. Off-by-small (1-2 lines) is tolerated; off-by-50+ becomes a weak signal."
            },
            "kind": {
                "type": "string",
                "description": "Symbol kind (function, method, class, ...). Weak tiebreaker only — cannot override file_path or line signals."
            },
            "language": {
                "type": "string",
                "description": "Language (typescript, rust, ...). Weakest signal, used only to break ties in multi-language repos."
            }
        }
    })
}

fn symbol_param_schema(string_desc: &str) -> serde_json::Value {
    json!({
        "oneOf": [
            {
                "type": "string",
                "description": string_desc
            },
            symbol_selector_schema()
        ]
    })
}

// ── Symbol tools ─────────────────────────────────────────────────────

fn make_symbol_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "search".into(),
            description: "Search symbols by name within a required project-relative scope. Scope is always required because it is both the result boundary and the focus seed; an existing CLI-built full index improves precision/performance but does not make scope optional.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query": { "type": "string", "description": "Search query text" },
                    "scope": { "type": "string", "description": "Required project-relative directory or file scope (e.g. 'drivers/net', 'src', 'kernel/sched'). Defines the search boundary and focus hotspot." },
                    "kind": { "type": "string", "description": "Optional SymbolKind filter (function, class, ...)" },
                    "limit": { "type": "integer", "description": "Max results (default 20)" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["query".into(), "scope".into()]),
            },
        },
        Tool {
            name: "symbol".into(),
            description: "Get symbol information by qualified name (symbol). view='detail' returns kind, location, and signature (with optional source via includeCode). view='context' returns structured callers, callees, file peers, imports, and dependencies. view='usages' returns reference usages. Default view is 'detail'.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. String matches are auto-resolved; use SymbolSelector object for precise disambiguation."),
                    "file_path": { "type": "string", "description": "File path relative to project root. When combined with 'line', resolves the symbol at this position (alternative to 'symbol' parameter)." },
                    "line": { "type": "integer", "description": "1-based line number. Used with 'file_path' for position-based symbol lookup." },
                    "column": { "type": "integer", "description": "1-based column number. Optional; defaults to 1 when omitted. Used with 'file_path' + 'line' for position-based symbol lookup." },
                    "view": {
                        "type": "string",
                        "enum": ["detail", "context", "usages"],
                        "description": "View mode: 'detail' for symbol info with optional source, 'context' for rich structured context, 'usages' for reference listing. Default: 'detail'."
                    },
                    "includeCode": { "type": "boolean", "description": "When true, includes the full source code of the enclosing definition (function/class/struct body). Default false (applies to view='detail' and 'context')." },
                    "includeFilePeers": { "type": "boolean", "description": "Include file peer symbols in context view (default: true). Set false for faster, smaller responses." },
                    "limit": { "type": "integer", "description": "Max results for view='usages' (default 50)." },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
    ]
}

// ── Graph tools ──────────────────────────────────────────────────────

fn make_graph_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "calls".into(),
            description: "Query the call graph around a symbol. direction='incoming' (callers) and 'outgoing' (callees) are fixed 1-hop and include signature when available; depth is ignored (warning). direction='both' enables multi-hop via depth (default 1, max 5). edge_kinds defaults to [\"calls\",\"instantiates\",\"implements\"]; use [\"*\"] for all kinds.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. Ambiguous matches are auto-aggregated. Use SymbolSelector object for a precise single-symbol query."),
                    "direction": {
                        "type": "string",
                        "enum": ["incoming", "outgoing", "both"],
                        "description": "Edge direction: 'incoming' for callers (1-hop), 'outgoing' for callees (1-hop), 'both' for multi-hop when depth>1 (default 'both')."
                    },
                    "depth": { "type": "integer", "description": "Only for direction=both: traversal depth (default 1, max 5). Ignored for incoming/outgoing (1-hop only)." },
                    "limit": { "type": "integer", "description": "Max nodes returned (default depends on mode)." },
                    "edge_kinds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Edge kinds to follow. Default: [\"calls\",\"instantiates\",\"implements\"]. Use [\"*\"] or [] for all edge kinds (neighbor query mode)."
                    },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "explore".into(),
            description: "Symbol dossier: investigate a symbol's identity, source code, call evidence with callsite snippets, non-call relations (implements, extends, references, field access, etc.), file context (imports/exports/peers), and recommended next queries. For multi-hop graph traversal use atlas_calls.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. Ambiguous matches return candidates. Use SymbolSelector object for precise disambiguation."),
                    "scope": { "type": "string", "description": "Optional project-relative directory or file scope for cold/local exploration (e.g. drivers/hid, net/smc). Keeps first-pass analysis bounded to the requested region." },
                    "source_mode": { "type": "string", "enum": ["excerpt", "full", "none"], "description": "Source display mode: excerpt (snippet around definition), full (entire symbol body, capped by max_source_bytes=65536), none (skip source). Default: excerpt." },
                    "source_lines": { "type": "integer", "description": "Max source lines to return when source_mode=excerpt. Default: 40." },
                    "evidence_limit": { "type": "integer", "description": "Max call evidence examples per direction. Default: 5." },
                    "relation_limit": { "type": "integer", "description": "Max non-call relation examples across all groups. Default: 12." },
                    "peer_limit": { "type": "integer", "description": "Max file peer symbols to return. Default: 12." },
                    "include_file_context": { "type": "boolean", "description": "Include imports, exports, and file peers. Default: true." },
                    "include_recommendations": { "type": "boolean", "description": "Include recommended next queries. Default: true." },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
        Tool {
            name: "path".into(),
            description: "Find the shortest path between two symbols through the graph (BFS). By default only follows call edges (calls, instantiates, implements, registers_callback). Use edge_kinds to override. Each edge hop includes direction (forward/reverse) and confidence. The path also includes breakpoints describing indirect hops, test code contamination, and reversed edges. Use prefer_production: true to prefer paths through production code over test files.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "from": symbol_param_schema("Source symbol qualified name. Ambiguous matches are auto-aggregated."),
                    "to": symbol_param_schema("Target symbol qualified name. Ambiguous matches are auto-aggregated."),
                    "max_depth": { "type": "integer", "description": "Max search depth (default 5, max 10)" },
                    "direction": {
                        "type": "string",
                        "enum": ["outgoing", "incoming", "both"],
                        "description": "Edge direction constraint during BFS: 'outgoing' (default) follows only forward/call edges, 'incoming' follows only reverse/caller edges, 'both' follows outgoing+incoming (use 'both' for reverse provenance / who-calls-X-to-reach-Y scenarios)."
                    },
                    "prefer_production": { "type": "boolean", "description": "When true, prefers paths through production (non-test) code. Test file nodes are deferred so production paths take priority even if longer by hop count. Default false." },
                    "edge_kinds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Edge kinds to follow. Default: [\"calls\", \"instantiates\", \"implements\", \"registers_callback\"]. Use [] or [\"*\"] for all edge kinds."
                    },
                    "includeCode": { "type": "boolean", "description": "When true, includes source code for each node in the path. Default false." },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: Some(vec!["from".into(), "to".into()]),
            },
        },
        Tool {
            name: "impact".into(),
            description: "Compute impact analysis: all symbols reachable from a given symbol via call graph traversal. Use direction='both' for bidirectional (downstream + upstream), direction='incoming' for callers only. Use semantic=true to include lifecycle invariants and branch diffs for impacted functions.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": symbol_param_schema("Qualified symbol name. Ambiguous matches are auto-aggregated."),
                    "direction": {
                        "type": "string",
                        "enum": ["outgoing", "incoming", "both"],
                        "description": "Traversal direction. 'outgoing' (default) follows forward/call edges only (downstream effects). 'incoming' follows reverse/caller edges only. 'both' follows both directions for full impact radius."
                    },
                    "depth": { "type": "integer", "description": "Max traversal depth (default 3, max 5)" },
                    "semantic": { "type": "boolean", "description": "When true, includes semantic impact analysis (lifecycle invariants, branch diffs) for impacted functions. Default false." },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
    ]
}

// ── File graph tools ─────────────────────────────────────────────────

fn make_file_graph_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "file_dependencies".into(),
            description: "Find file-level dependencies by project-relative path. direction='outgoing' lists files that this file imports/includes, 'incoming' lists files that import/include this file, 'both' returns both directions. file_path is required (project-relative, no file_id).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "file_path": { "type": "string", "description": "Project-relative file path (e.g. 'src/main.rs'). Required." },
                    "direction": {
                        "type": "string",
                        "enum": ["incoming", "outgoing", "both"],
                        "description": "Direction: 'outgoing' (default) for imports by this file, 'incoming' for files importing this file, 'both' for both directions."
                    },
                    "limit": { "type": "integer", "description": "Max results (default 50)." },
                    "analysis": {
                        "type": "string",
                        "enum": ["manifest", "structural"],
                        "description": "Analysis mode: 'manifest' (default, fast — uses existing DB facts, no lazy extraction) vs 'structural' (bounded lazy refinement for better coverage).",
                        "default": "manifest"
                    },
                })),
                required: Some(vec!["file_path".into()]),
            },
        },
    ]
}

// ── Trace tools ──────────────────────────────────────────────────────

pub(crate) fn make_trace_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "trace".into(),
            description: "Source-level trace queries. kind='point' resolves a source position (file+line+column) to its full context. kind='variable' traces where a variable's value comes from (backward dataflow). kind='forward' traces the forward call chain from source to target. kind='callers' traces how a function gets invoked (backward call chain to farthest caller). Use file_id (hex) or file_path (project-relative) for position-based kinds; use symbol for kind='callers'; use from/to for kind='forward'.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "kind": {
                        "type": "string",
                        "description": "Trace operation kind.",
                        "oneOf": [
                            {
                                "const": "point",
                                "description": "Resolve a source position (file+line+column) to its full context — enclosing symbol, reference, scope, data node, and callsite. Triggers scoped structural/dataflow preparation when needed; capability gaps are reported in the response."
                            },
                            {
                                "const": "variable",
                                "description": "Trace where a variable's value comes from (backward intra-procedural dataflow). Requires dataflow layer for complete results; returns best-effort on structural-only projects."
                            },
                            {
                                "const": "forward",
                                "description": "Trace the forward call chain from source symbol to target symbol. Scoped focus prepares call-graph edges when needed; partial coverage is reported in the response."
                            },
                            {
                                "const": "callers",
                                "description": "Trace how a function gets invoked — backward call chain to the farthest caller. Scoped focus prepares call-graph edges when needed; partial coverage is reported in the response."
                            }
                        ]
                    },
                    "file_id": { "type": "string", "description": "File ID in hex (alternative to file_path for kind='point'/'variable')." },
                    "file_path": { "type": "string", "description": "File path relative to project root (e.g. 'src/foo.ts'). Alternative to file_id." },
                    "line": { "type": "integer", "description": "1-based line number (required for kind='point'/'variable')." },
                    "column": { "type": "integer", "description": "1-based column number (required for kind='point'/'variable')." },
                    "symbol": symbol_param_schema("Qualified symbol name. Use SymbolSelector object for precise disambiguation."),
                    "from": symbol_param_schema("Source qualified symbol name."),
                    "to": symbol_param_schema("Target qualified symbol name."),
                    "max_depth": { "type": "integer", "description": "Maximum traversal depth (kind='variable'/'forward'/'callers')." },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional request-scoped C/C++ include search roots (project-relative). Used only for lazy include resolution in this call; not persisted. Example: [\"include\", \"third_party/include\"]" },
                })),
                required: None,
            },
        },
    ]
}

// ── Semantic analysis tools ──────────────────────────────────────────

fn make_semantic_analysis_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "lifecycle".into(),
            description: "Analyze a field's lifecycle within a function using CFG effect annotations (C/C++). Walks the control-flow graph to track a field through allocate → use → free transitions, detecting use-after-free, double-free, and missing-free patterns. Triggers lazy structural extraction if CFG not yet built.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified function name to analyze (e.g. 'handle_request')" },
                    "field": { "type": "string", "description": "Field path to track (e.g. 'data->state.ptr' for C/C++ struct field access)" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional C/C++ include roots" },
                })),
                required: Some(vec!["symbol".into(), "field".into()]),
            },
        },
        Tool {
            name: "branch_diff".into(),
            description: "Compare side effects of sibling branches (if/else, switch) within a function. Detects suspicious asymmetries — e.g., one branch frees a field but the other does not. Uses CFG effect annotations (C/C++ only initially).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "symbol": { "type": "string", "description": "Qualified function name to analyze" },
                    "include_roots": { "type": "array", "items": { "type": "string" }, "description": "Optional C/C++ include roots" },
                })),
                required: Some(vec!["symbol".into()]),
            },
        },
    ]
}

// ── Domain rules tools (semantic analysis) ──────────────────────────

fn make_domain_rules_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "domain_rules".into(),
            description: "Manage domain rules for lifecycle analysis. action='add' defines which functions allocate/free/own memory (required: rule_kind [free_fn|alloc_fn|owned_pattern|cleanup_fn], pattern). action='list' shows rules, optionally filtered by source (builtin/learned/user). action='delete' removes a rule (required: rule_id). action='learn' auto-discovers rule candidates from project patterns (optional: min_confidence).".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "action": {
                        "type": "string",
                        "enum": ["add", "list", "delete", "learn"],
                        "description": "Action: 'add' to define a rule, 'list' to show rules, 'delete' to remove a rule, 'learn' to discover candidates."
                    },
                    "rule_kind": {
                        "type": "string",
                        "enum": ["free_fn", "alloc_fn", "owned_pattern", "cleanup_fn"],
                        "description": "Rule kind (required for action='add')."
                    },
                    "pattern": { "type": "string", "description": "Function name or field pattern (required for action='add')." },
                    "rule_id": { "type": "string", "description": "Rule ID (required for action='delete')." },
                    "source": { "type": "string", "enum": ["builtin", "learned", "user"], "description": "Filter by source (optional for action='list')." },
                    "confidence": { "type": "number", "description": "Confidence 0.0-1.0 (default 1.0 for user-declared)." },
                    "min_confidence": { "type": "number", "description": "Minimum confidence threshold for action='learn' (default 0.5)." },
                })),
                required: None,
            },
        },
    ]
}

// ── FP dispatch tools (C/C++) ───────────────────────────────────────

fn make_fp_dispatch_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "fp_dispatches".into(),
            description: "Manage function-pointer dispatch annotations for C/C++ code. action='add' declares a mapping from a struct's function-pointer field to its concrete target function (required: field_qname, target_qname). action='list' returns all declared annotations. action='delete' removes an annotation (required: annotation_id OR field_qname). Annotations are stored in the active project database; graph edges are materialized immediately.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "action": {
                        "type": "string",
                        "enum": ["add", "list", "delete"],
                        "description": "Action: 'add' to declare a dispatch, 'list' to show all annotations, 'delete' to remove one."
                    },
                    "field_qname": { "type": "string", "description": "Qualified name of the function-pointer field (required for action='add'; alternative identifier for action='delete')." },
                    "target_qname": { "type": "string", "description": "Qualified name of the target function (required for action='add')." },
                    "annotation_id": { "type": "string", "description": "Annotation ID from list (alternative identifier for action='delete')." },
                    "confidence": { "type": "number", "description": "Confidence score 0.0-1.0 (default 1.0 for user-declared)." },
                })),
                required: None,
            },
        },
    ]
}

// ── Query/job tools ──────────────────────────────────────────────────

fn make_task_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "tasks".into(),
            description: "List focus/lazy extraction jobs and query refinement state. Without arguments, lists all active jobs. Use query_id to filter refinement work triggered by a specific query.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query_id": { "type": "string", "description": "Optional query_id to filter jobs." },
                })),
                required: None,
            },
        },
        Tool {
            name: "resume_query".into(),
            description: "Re-run a previous query snapshot to get enhanced results after focus/lazy refinement. Returns the same format as the original tool with potentially richer data.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({
                    "query_id": { "type": "string", "description": "The query_id from a previous tool call response" },
                })),
                required: Some(vec!["query_id".into()]),
            },
        },
    ]
}

pub fn make_all_tools() -> Vec<Tool> {
    let mut tools = Vec::new();
    tools.extend(make_project_tools());
    tools.extend(make_symbol_tools());
    tools.extend(make_graph_tools());
    tools.extend(make_file_graph_tools());
    tools.extend(make_trace_tools());
    tools.extend(make_semantic_analysis_tools());
    tools.extend(make_domain_rules_tools());
    tools.extend(make_fp_dispatch_tools());
    tools.extend(make_task_tools());
    tools
}

/// Merge edge-based file references into a dependents/dependencies JSON value.
pub(crate) fn merge_edge_deps(
    value: &mut serde_json::Value,
    edge_deps: &serde_json::Value,
    list_field: &str,
    total_field: &str,
) {
    if let Some(arr) = edge_deps.as_array() {
        if arr.is_empty() {
            return;
        }
        if let Some(deps) = value.get_mut(list_field) {
            if let Some(existing) = deps.as_array_mut() {
                for dep in arr {
                    existing.push(dep.clone());
                }
            }
        }
        if let Some(total) = value.get_mut(total_field) {
            if let Some(n) = total.as_u64() {
                *total = serde_json::json!(n + arr.len() as u64);
            }
        }
    }
}
