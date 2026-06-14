//! Tool contract system for v6.0 runtime architecture.
//!
//! Each MCP tool is classified into a `ToolContract` that specifies:
//! - Which runtime should handle it
//! - What data needs must be satisfied before execution
//! - What side effects are expected
//!
//! This replaces the flat `match` dispatch in v5.0 ToolRouter::call_tool().

use serde_json::Value;

/// The contract for a tool call — determines which runtime handles it
/// and what resources must be prepared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolContract {
    /// Open/close/configure a project. Only handle_project (action=open).
    ProjectLifecycle,

    /// Read-only status queries. project (action=status/files).
    StatusRead,

    /// Graph-backed queries that need the call graph.
    /// calls, explore, path, impact, symbol(view=context)
    SemanticGraphQuery(QueryNeeds),

    /// Source-level trace operations (point, variable, forward, callers).
    /// Uses atlas_engine::Engine directly — does NOT require GraphSnapshot.
    TraceQuery(QueryNeeds),

    /// Store-fact queries that read symbol/file data directly.
    /// symbol(detail/usages), file_dependencies, search
    StoreFactQuery(QueryNeeds),

    /// Semantic analysis requiring CFG/dataflow facts.
    /// branch_diff, lifecycle
    SemanticAnalysis(AnalysisNeeds),

    /// Mutation of overlay state (persisted annotations).
    /// fp_dispatches (action=add/delete), domain_rules (action=add/delete)
    OverlayMutation(OverlayKind),

    /// Read-only overlay queries.
    /// fp_dispatches (action=list), domain_rules (action=list)
    OverlayRead,

    /// Query refinement/job observability.
    /// tasks, resume_query
    TaskControl,
}

/// What level of data the query needs before it can produce a useful result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryNeeds {
    /// Only symbol names and basic metadata.
    Manifest,
    /// Imports, references, symbol relationships.
    Structural,
    /// Resolved call edges + full graph topology.
    CallGraph,
    /// Everything including dataflow.
    Full,
}

/// What CFG/dataflow preparations an analysis tool needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisNeeds {
    /// Control flow graph nodes and edges only.
    #[allow(dead_code)]
    Cfg,
    /// CFG + dataflow nodes and edges.
    #[allow(dead_code)]
    CfgAndDataflow,
    /// CFG + dataflow + composed effects.
    CfgDataflowEffects,
    /// CFG + dataflow + domain rules for ownership classification.
    CfgDataflowDomainRules,
}

/// Kind of overlay annotation being mutated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayKind {
    /// Function pointer dispatch annotations.
    FunctionPointerDispatch,
    /// Domain rules (ownership, allocation patterns, etc.).
    DomainRules,
}

/// Map a tool name and its arguments to a ToolContract.
///
/// This is the single source of truth for v6.0 routing.
/// When a new tool is added, add a mapping here — the dispatch
/// layer will automatically route it to the correct runtime.
pub fn contract_for(tool_name: &str, args: &Value) -> ToolContract {
    match tool_name {
        // ── Project lifecycle ──
        "project" if args.get("action").and_then(|v| v.as_str()) == Some("open") => {
            ToolContract::ProjectLifecycle
        }
        "project" => ToolContract::StatusRead,

        // ── Graph-backed semantic queries ──
        "calls" => ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph),
        "explore" => ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph),
        "path" => ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph),
        "impact" => ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph),
        "symbol" if args.get("view").and_then(|v| v.as_str()) == Some("context") => {
            ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph)
        }
        "symbol" => ToolContract::StoreFactQuery(QueryNeeds::Manifest),

        // ── Store-fact queries ──
        "search" => ToolContract::StoreFactQuery(QueryNeeds::Manifest),
        "file_dependencies" => ToolContract::StoreFactQuery(QueryNeeds::Structural),

        // ── Trace (Engine-driven, no GraphSnapshot needed) ──
        "trace" => ToolContract::TraceQuery(QueryNeeds::Full),

        // ── Semantic analysis ──
        "branch_diff" => ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowEffects),
        "lifecycle" => ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowDomainRules),

        // ── Overlay mutations ──
        "fp_dispatches" if is_mutation(args) => {
            ToolContract::OverlayMutation(OverlayKind::FunctionPointerDispatch)
        }
        "fp_dispatches" => ToolContract::OverlayRead,

        "domain_rules" if is_mutation(args) => {
            ToolContract::OverlayMutation(OverlayKind::DomainRules)
        }
        "domain_rules" => ToolContract::OverlayRead,

        // ── Query/job control ──
        "tasks" | "resume_query" => ToolContract::TaskControl,

        // ── Unknown ──
        _ => ToolContract::StatusRead,
    }
}

/// Return true for mutation actions (add, delete, enable, disable).
fn is_mutation(args: &Value) -> bool {
    match args.get("action").and_then(|v| v.as_str()) {
        Some("add") | Some("delete") | Some("enable") | Some("disable") => true,
        _ => false,
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_project_open_is_lifecycle() {
        let c = contract_for(
            "project",
            &json!({"action": "open", "project_path": "/tmp"}),
        );
        assert_eq!(c, ToolContract::ProjectLifecycle);
    }

    #[test]
    fn test_project_status_is_read() {
        let c = contract_for("project", &json!({"action": "status"}));
        assert_eq!(c, ToolContract::StatusRead);
    }

    #[test]
    fn test_project_files_is_read() {
        let c = contract_for("project", &json!({"action": "files"}));
        assert_eq!(c, ToolContract::StatusRead);
    }

    #[test]
    fn test_calls_is_semantic_graph() {
        let c = contract_for("calls", &json!({"symbol": "foo"}));
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph));
    }

    #[test]
    fn test_explore_is_semantic_graph() {
        let c = contract_for("explore", &json!({"symbol": "foo"}));
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph));
    }

    #[test]
    fn test_path_is_semantic_graph() {
        let c = contract_for("path", &json!({"from": "a", "to": "b"}));
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph));
    }

    #[test]
    fn test_impact_is_semantic_graph() {
        let c = contract_for("impact", &json!({"symbol": "foo"}));
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph));
    }

    #[test]
    fn test_symbol_context_is_semantic_graph() {
        let c = contract_for("symbol", &json!({"symbol": "foo", "view": "context"}));
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeeds::CallGraph));
    }

    #[test]
    fn test_symbol_detail_is_store_fact() {
        let c = contract_for("symbol", &json!({"symbol": "foo"}));
        assert_eq!(c, ToolContract::StoreFactQuery(QueryNeeds::Manifest));
    }

    #[test]
    fn test_symbol_usages_is_store_fact() {
        let c = contract_for("symbol", &json!({"symbol": "foo", "view": "usages"}));
        assert_eq!(c, ToolContract::StoreFactQuery(QueryNeeds::Manifest));
    }

    #[test]
    fn test_search_is_store_fact() {
        let c = contract_for("search", &json!({"query": "foo"}));
        assert_eq!(c, ToolContract::StoreFactQuery(QueryNeeds::Manifest));
    }

    #[test]
    fn test_file_dependencies_is_store_fact_structural() {
        let c = contract_for("file_dependencies", &json!({"file_path": "src/main.rs"}));
        assert_eq!(c, ToolContract::StoreFactQuery(QueryNeeds::Structural));
    }

    #[test]
    fn test_trace_is_trace_query() {
        let c = contract_for(
            "trace",
            &json!({"kind": "point", "file_path": "x.rs", "line": 1, "column": 1}),
        );
        assert_eq!(c, ToolContract::TraceQuery(QueryNeeds::Full));
    }

    #[test]
    fn test_branch_diff_is_analysis() {
        let c = contract_for("branch_diff", &json!({"symbol": "foo"}));
        assert_eq!(
            c,
            ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowEffects)
        );
    }

    #[test]
    fn test_lifecycle_is_analysis() {
        let c = contract_for("lifecycle", &json!({"symbol": "foo", "field": "ptr"}));
        assert_eq!(
            c,
            ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowDomainRules)
        );
    }

    #[test]
    fn test_fp_dispatches_add_is_mutation() {
        let c = contract_for(
            "fp_dispatches",
            &json!({"action": "add", "field_qname": "foo", "target_qname": "bar"}),
        );
        assert_eq!(
            c,
            ToolContract::OverlayMutation(OverlayKind::FunctionPointerDispatch)
        );
    }

    #[test]
    fn test_fp_dispatches_list_is_read() {
        let c = contract_for("fp_dispatches", &json!({"action": "list"}));
        assert_eq!(c, ToolContract::OverlayRead);
    }

    #[test]
    fn test_fp_dispatches_default_is_read() {
        let c = contract_for("fp_dispatches", &json!({}));
        assert_eq!(c, ToolContract::OverlayRead);
    }

    #[test]
    fn test_domain_rules_add_is_mutation() {
        let c = contract_for(
            "domain_rules",
            &json!({"action": "add", "language": "cpp", "rule_kind": "free_fn", "pattern": "xfree"}),
        );
        assert_eq!(c, ToolContract::OverlayMutation(OverlayKind::DomainRules));
    }

    #[test]
    fn test_domain_rules_delete_is_mutation() {
        let c = contract_for(
            "domain_rules",
            &json!({"action": "delete", "rule_id": "abc"}),
        );
        assert_eq!(c, ToolContract::OverlayMutation(OverlayKind::DomainRules));
    }

    #[test]
    fn test_domain_rules_list_is_read() {
        let c = contract_for("domain_rules", &json!({"action": "list"}));
        assert_eq!(c, ToolContract::OverlayRead);
    }

    #[test]
    fn test_tasks_is_control() {
        let c = contract_for("tasks", &json!({}));
        assert_eq!(c, ToolContract::TaskControl);
    }

    #[test]
    fn test_resume_query_is_control() {
        let c = contract_for("resume_query", &json!({"query_id": "abc"}));
        assert_eq!(c, ToolContract::TaskControl);
    }

    #[test]
    fn test_unknown_tool_is_status_read() {
        let c = contract_for("nonexistent_tool", &json!({}));
        assert_eq!(c, ToolContract::StatusRead);
    }

    #[test]
    fn test_is_mutation_actions() {
        assert!(is_mutation(&json!({"action": "add"})));
        assert!(is_mutation(&json!({"action": "delete"})));
        assert!(is_mutation(&json!({"action": "enable"})));
        assert!(is_mutation(&json!({"action": "disable"})));
        assert!(!is_mutation(&json!({"action": "list"})));
        assert!(!is_mutation(&json!({"action": "learn"})));
        assert!(!is_mutation(&json!({})));
    }
}
