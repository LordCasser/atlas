//! Schema validation tests — catch MCP tool schema drift.
//!
//! These tests verify that critical parameters remain present in the
//! tool input schemas returned by `make_all_tools()`. If you add or
//! remove a parameter intentionally, update the corresponding test here.
//!
//! Run with: `cargo test -p atlas-mcp -- schema`

use atlas_mcp::make_all_tools;
use std::collections::BTreeSet;

#[test]
fn v1_tool_names_are_frozen() {
    let tools = make_all_tools();
    let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "branch_diff",
            "calls",
            "domain_rules",
            "explore",
            "file_dependencies",
            "fp_dispatches",
            "impact",
            "lifecycle",
            "path",
            "project",
            "resume_query",
            "search",
            "symbol",
            "tasks",
            "trace",
        ]
    );
}

#[test]
fn v1_tool_argument_shapes_are_frozen() {
    let tools = make_all_tools();
    let expected = [
        (
            "branch_diff",
            ["include_roots", "symbol"].as_slice(),
            ["symbol"].as_slice(),
        ),
        (
            "calls",
            [
                "depth",
                "direction",
                "edge_kinds",
                "include_roots",
                "limit",
                "symbol",
            ]
            .as_slice(),
            ["symbol"].as_slice(),
        ),
        (
            "domain_rules",
            [
                "action",
                "confidence",
                "min_confidence",
                "pattern",
                "rule_id",
                "rule_kind",
                "source",
            ]
            .as_slice(),
            [].as_slice(),
        ),
        (
            "explore",
            [
                "evidence_limit",
                "include_file_context",
                "include_recommendations",
                "include_roots",
                "peer_limit",
                "relation_limit",
                "scope",
                "source_lines",
                "source_mode",
                "symbol",
            ]
            .as_slice(),
            ["symbol"].as_slice(),
        ),
        (
            "file_dependencies",
            ["analysis", "direction", "file_path", "limit"].as_slice(),
            ["file_path"].as_slice(),
        ),
        (
            "fp_dispatches",
            [
                "action",
                "annotation_id",
                "confidence",
                "field_qname",
                "target_qname",
            ]
            .as_slice(),
            [].as_slice(),
        ),
        (
            "impact",
            ["depth", "direction", "semantic", "symbol"].as_slice(),
            ["symbol"].as_slice(),
        ),
        (
            "lifecycle",
            ["field", "include_roots", "symbol"].as_slice(),
            ["symbol", "field"].as_slice(),
        ),
        (
            "path",
            [
                "direction",
                "edge_kinds",
                "from",
                "includeCode",
                "include_roots",
                "max_depth",
                "prefer_production",
                "to",
            ]
            .as_slice(),
            ["from", "to"].as_slice(),
        ),
        (
            "project",
            [
                "action",
                "language",
                "limit",
                "path_prefix",
                "project_path",
                "verbose",
            ]
            .as_slice(),
            [].as_slice(),
        ),
        (
            "resume_query",
            ["query_id"].as_slice(),
            ["query_id"].as_slice(),
        ),
        (
            "search",
            ["include_roots", "kind", "limit", "query", "scope"].as_slice(),
            ["query", "scope"].as_slice(),
        ),
        (
            "symbol",
            [
                "column",
                "file_path",
                "includeCode",
                "includeFilePeers",
                "include_roots",
                "limit",
                "line",
                "symbol",
                "view",
            ]
            .as_slice(),
            ["symbol"].as_slice(),
        ),
        ("tasks", ["query_id"].as_slice(), [].as_slice()),
        (
            "trace",
            [
                "column",
                "file_id",
                "file_path",
                "from",
                "include_roots",
                "kind",
                "line",
                "max_depth",
                "symbol",
                "to",
            ]
            .as_slice(),
            [].as_slice(),
        ),
    ];

    for (name, expected_props, expected_required) in expected {
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} tool must exist"));
        let actual_props: BTreeSet<&str> = tool
            .input_schema
            .properties
            .as_ref()
            .and_then(|props| props.as_object())
            .unwrap_or_else(|| panic!("{name} tool must have object properties"))
            .keys()
            .map(String::as_str)
            .collect();
        let expected_props: BTreeSet<&str> = expected_props.iter().copied().collect();
        assert_eq!(
            actual_props, expected_props,
            "{name} V1 schema properties changed"
        );

        let actual_required: BTreeSet<&str> = tool
            .input_schema
            .required
            .as_ref()
            .map(|required| required.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let expected_required: BTreeSet<&str> = expected_required.iter().copied().collect();
        assert_eq!(
            actual_required, expected_required,
            "{name} V1 required fields changed"
        );
    }
}

#[test]
fn removed_background_index_tools_are_not_registered() {
    let tools = make_all_tools();
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    for removed in ["index", "task_status", "wait_for_task", "resume_task"] {
        assert!(
            !names.contains(&removed),
            "removed MCP tool '{removed}' must not be registered"
        );
    }
    assert!(
        names.contains(&"resume_query"),
        "resume_query must replace resume_task"
    );
}

#[test]
fn schema_search_requires_scope_and_has_no_background_flag() {
    let tools = make_all_tools();
    let search_tool = tools
        .iter()
        .find(|t| t.name == "search")
        .expect("search tool must exist");

    let required = search_tool
        .input_schema
        .required
        .as_ref()
        .expect("search tool must have required fields");
    assert!(required.iter().any(|r| r == "query"));
    assert!(
        required.iter().any(|r| r == "scope"),
        "search scope must be required"
    );

    let props = search_tool
        .input_schema
        .properties
        .as_ref()
        .expect("search tool must have properties in its schema");
    assert!(
        props.get("background").is_none(),
        "search.background was removed from MCP semantics"
    );
}

#[test]
fn schema_project_open_has_no_background_index_parameters() {
    let tools = make_all_tools();
    let project_tool = tools
        .iter()
        .find(|t| t.name == "project")
        .expect("project tool must exist");

    let props = project_tool
        .input_schema
        .properties
        .as_ref()
        .expect("project tool must have properties in its schema");
    for removed in ["background", "scan_files", "force_memory", "storage"] {
        assert!(
            props.get(removed).is_none(),
            "project.{removed} must not be exposed in MCP schema"
        );
    }
}

#[test]
fn schema_tasks_observes_queries_not_async_tasks() {
    let tools = make_all_tools();
    let tasks_tool = tools
        .iter()
        .find(|t| t.name == "tasks")
        .expect("tasks tool must exist");

    let props = tasks_tool
        .input_schema
        .properties
        .as_ref()
        .and_then(|props| props.as_object())
        .expect("tasks tool must have object properties");
    assert!(
        props.get("query_id").is_some(),
        "tasks must keep query_id-based refinement observability"
    );
    assert!(
        props.get("task_id").is_none(),
        "tasks must not expose a separate async task polling surface"
    );
}

#[test]
fn schema_graph_tools_accept_request_scoped_include_roots() {
    let tools = make_all_tools();
    for name in [
        "calls",
        "explore",
        "path",
        "trace",
        "lifecycle",
        "branch_diff",
    ] {
        let tool = tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("{name} tool must exist"));
        let props = tool
            .input_schema
            .properties
            .as_ref()
            .and_then(|props| props.as_object())
            .unwrap_or_else(|| panic!("{name} tool must have object properties"));
        assert!(
            props.get("include_roots").is_some(),
            "{name} must expose request-scoped include_roots in its MCP schema"
        );
    }
}

#[test]
fn all_tools_have_descriptions() {
    let tools = make_all_tools();
    for tool in &tools {
        assert!(
            !tool.description.is_empty(),
            "Tool '{}' has empty description",
            tool.name
        );
    }
}

/// Print a warning for any tool with a suspiciously simple schema.
/// This is non-fatal — only prints to stdout for manual review.
#[test]
fn schema_warn_on_incomplete_tool_schemas() {
    let tools = make_all_tools();
    for tool in &tools {
        let has_props = tool
            .input_schema
            .properties
            .as_ref()
            .and_then(|p| p.as_object())
            .map(|o| !o.is_empty())
            .unwrap_or(false);
        if !has_props {
            println!(
                "WARNING: Tool '{}' has no properties in its input schema",
                tool.name
            );
        }
    }
}
