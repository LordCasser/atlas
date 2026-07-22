//! Tool contract system for v6.0 runtime architecture.
//!
//! Each MCP tool is classified into a `ToolContract` that specifies:
//! - Which runtime should handle it
//! - What data needs must be satisfied before execution
//! - What side effects are expected
//!
//! This replaces the flat `match` dispatch in v5.0 ToolRouter::call_tool().

pub use atlas_engine::QueryNeed;
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
    SemanticGraphQuery(QueryNeed),

    /// Source-level trace operations (point, variable, forward, callers).
    /// Uses atlas_engine::Engine directly — does NOT require GraphSnapshot.
    TraceQuery(QueryNeed),

    /// Store-fact queries that read symbol/file data directly.
    /// symbol(detail/usages), file_dependencies, search
    StoreFactQuery(QueryNeed),

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

/// What CFG/dataflow preparations an analysis tool needs.
///
/// Only variants used by live tools. Dead "Cfg-only" / "CfgAndDataflow" stages
/// were removed (no tools gated that way; DEBT-8 contract fingerprint).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalysisNeeds {
    /// CFG + dataflow + composed effects (`branch_diff`).
    CfgDataflowEffects,
    /// CFG + dataflow + domain rules for ownership (`lifecycle`).
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

impl ToolContract {
    /// Strongest fact layer required before the tool may publish result data.
    pub fn query_need(&self) -> Option<QueryNeed> {
        match self {
            Self::SemanticGraphQuery(need)
            | Self::TraceQuery(need)
            | Self::StoreFactQuery(need) => Some(*need),
            Self::SemanticAnalysis(_) => Some(QueryNeed::Dataflow),
            _ => None,
        }
    }
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
        "calls" => ToolContract::SemanticGraphQuery(QueryNeed::CallGraph),
        "explore" => ToolContract::SemanticGraphQuery(QueryNeed::CallGraph),
        "path" => ToolContract::SemanticGraphQuery(QueryNeed::CallGraph),
        "impact" => ToolContract::SemanticGraphQuery(
            if args
                .get("semantic")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                QueryNeed::Dataflow
            } else {
                QueryNeed::CallGraph
            },
        ),
        "symbol" if args.get("view").and_then(|v| v.as_str()) == Some("context") => {
            ToolContract::SemanticGraphQuery(QueryNeed::CallGraph)
        }
        "symbol" if args.get("view").and_then(Value::as_str) == Some("usages") => {
            ToolContract::StoreFactQuery(QueryNeed::CallGraph)
        }
        "symbol" if args.get("includeCode").and_then(Value::as_bool) == Some(true) => {
            ToolContract::StoreFactQuery(QueryNeed::Structural)
        }
        "symbol" => ToolContract::StoreFactQuery(QueryNeed::Structural),

        // ── Store-fact queries ──
        "search" => ToolContract::StoreFactQuery(QueryNeed::Structural),
        "file_dependencies" => ToolContract::StoreFactQuery(
            if args.get("analysis").and_then(Value::as_str) == Some("structural") {
                QueryNeed::CallGraph
            } else {
                QueryNeed::Manifest
            },
        ),

        // ── Trace (Engine-driven, no GraphSnapshot needed) ──
        "trace" => ToolContract::TraceQuery(
            match args.get("kind").and_then(Value::as_str).unwrap_or("point") {
                "variable" => QueryNeed::Dataflow,
                "forward" | "callers" => QueryNeed::CallGraph,
                _ => QueryNeed::Structural,
            },
        ),

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
    matches!(
        args.get("action").and_then(|v| v.as_str()),
        Some("add") | Some("delete") | Some("enable") | Some("disable")
    )
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
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeed::CallGraph));
    }

    #[test]
    fn test_explore_is_semantic_graph() {
        let c = contract_for("explore", &json!({"symbol": "foo"}));
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeed::CallGraph));
    }

    #[test]
    fn test_path_is_semantic_graph() {
        let c = contract_for("path", &json!({"from": "a", "to": "b"}));
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeed::CallGraph));
    }

    #[test]
    fn test_impact_is_semantic_graph() {
        let c = contract_for("impact", &json!({"symbol": "foo"}));
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeed::CallGraph));

        let semantic = contract_for("impact", &json!({"symbol": "foo", "semantic": true}));
        assert_eq!(
            semantic,
            ToolContract::SemanticGraphQuery(QueryNeed::Dataflow)
        );
    }

    #[test]
    fn test_symbol_context_is_semantic_graph() {
        let c = contract_for("symbol", &json!({"symbol": "foo", "view": "context"}));
        assert_eq!(c, ToolContract::SemanticGraphQuery(QueryNeed::CallGraph));
    }

    #[test]
    fn test_symbol_detail_is_store_fact() {
        let c = contract_for("symbol", &json!({"symbol": "foo"}));
        assert_eq!(c, ToolContract::StoreFactQuery(QueryNeed::Structural));

        let with_source = contract_for("symbol", &json!({"symbol": "foo", "includeCode": true}));
        assert_eq!(
            with_source,
            ToolContract::StoreFactQuery(QueryNeed::Structural)
        );
    }

    #[test]
    fn test_symbol_usages_is_store_fact() {
        let c = contract_for("symbol", &json!({"symbol": "foo", "view": "usages"}));
        assert_eq!(c, ToolContract::StoreFactQuery(QueryNeed::CallGraph));
    }

    #[test]
    fn test_search_is_store_fact() {
        let c = contract_for("search", &json!({"query": "foo"}));
        assert_eq!(c, ToolContract::StoreFactQuery(QueryNeed::Structural));
    }

    #[test]
    fn test_file_dependencies_is_store_fact_structural() {
        let manifest = contract_for("file_dependencies", &json!({"file_path": "src/main.rs"}));
        assert_eq!(manifest, ToolContract::StoreFactQuery(QueryNeed::Manifest));

        let c = contract_for(
            "file_dependencies",
            &json!({"file_path": "src/main.rs", "analysis": "structural"}),
        );
        assert_eq!(c, ToolContract::StoreFactQuery(QueryNeed::CallGraph));
    }

    #[test]
    fn test_trace_is_trace_query() {
        let point = contract_for(
            "trace",
            &json!({"kind": "point", "file_path": "x.rs", "line": 1, "column": 1}),
        );
        assert_eq!(point, ToolContract::TraceQuery(QueryNeed::Structural));
        assert_eq!(
            contract_for("trace", &json!({"kind": "variable"})),
            ToolContract::TraceQuery(QueryNeed::Dataflow)
        );
        assert_eq!(
            contract_for("trace", &json!({"kind": "forward"})),
            ToolContract::TraceQuery(QueryNeed::CallGraph)
        );
        assert_eq!(
            contract_for("trace", &json!({"kind": "callers"})),
            ToolContract::TraceQuery(QueryNeed::CallGraph)
        );
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

    /// Every live tool name maps to a non-default contract path used by dispatch.
    #[test]
    fn contract_covers_all_v1_tool_names() {
        let tools: &[(&str, Value, ToolContract)] = &[
            (
                "project",
                json!({"action": "open", "project_path": "/tmp"}),
                ToolContract::ProjectLifecycle,
            ),
            (
                "project",
                json!({"action": "status"}),
                ToolContract::StatusRead,
            ),
            (
                "calls",
                json!({"symbol": "f"}),
                ToolContract::SemanticGraphQuery(QueryNeed::CallGraph),
            ),
            (
                "explore",
                json!({"symbol": "f"}),
                ToolContract::SemanticGraphQuery(QueryNeed::CallGraph),
            ),
            (
                "path",
                json!({"from": "a", "to": "b"}),
                ToolContract::SemanticGraphQuery(QueryNeed::CallGraph),
            ),
            (
                "impact",
                json!({"symbol": "f"}),
                ToolContract::SemanticGraphQuery(QueryNeed::CallGraph),
            ),
            (
                "symbol",
                json!({"symbol": "f", "view": "context"}),
                ToolContract::SemanticGraphQuery(QueryNeed::CallGraph),
            ),
            (
                "symbol",
                json!({"symbol": "f"}),
                ToolContract::StoreFactQuery(QueryNeed::Structural),
            ),
            (
                "search",
                json!({"query": "x"}),
                ToolContract::StoreFactQuery(QueryNeed::Structural),
            ),
            (
                "file_dependencies",
                json!({"file_path": "a.rs"}),
                ToolContract::StoreFactQuery(QueryNeed::Manifest),
            ),
            (
                "trace",
                json!({"kind": "point", "file_path": "a.rs", "line": 1, "column": 1}),
                ToolContract::TraceQuery(QueryNeed::Structural),
            ),
            (
                "branch_diff",
                json!({"symbol": "f"}),
                ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowEffects),
            ),
            (
                "lifecycle",
                json!({"symbol": "f", "field": "p"}),
                ToolContract::SemanticAnalysis(AnalysisNeeds::CfgDataflowDomainRules),
            ),
            (
                "fp_dispatches",
                json!({"action": "list"}),
                ToolContract::OverlayRead,
            ),
            (
                "domain_rules",
                json!({"action": "list"}),
                ToolContract::OverlayRead,
            ),
            ("tasks", json!({}), ToolContract::TaskControl),
            (
                "resume_query",
                json!({"query_id": "q"}),
                ToolContract::TaskControl,
            ),
        ];
        for (name, args, expected) in tools {
            assert_eq!(contract_for(name, args), *expected, "contract_for({name})");
        }
    }

    /// AnalysisNeeds has only live variants (no dead Cfg / CfgAndDataflow).
    #[test]
    fn analysis_needs_only_live_variants() {
        // Exhaustive match — compile fails if a variant is re-added without updating.
        for needs in [
            AnalysisNeeds::CfgDataflowEffects,
            AnalysisNeeds::CfgDataflowDomainRules,
        ] {
            match needs {
                AnalysisNeeds::CfgDataflowEffects | AnalysisNeeds::CfgDataflowDomainRules => {}
            }
        }
    }
}
