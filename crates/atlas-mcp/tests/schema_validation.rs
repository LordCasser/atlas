//! Schema validation tests — catch MCP tool schema drift.
//!
//! These tests verify that critical parameters remain present in the
//! tool input schemas returned by `make_all_tools()`. If you add or
//! remove a parameter intentionally, update the corresponding test here.
//!
//! Run with: `cargo test -p atlas-mcp -- schema`

use atlas_mcp::make_all_tools;

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
    for removed in ["background", "scan_files", "force_memory"] {
        assert!(
            props.get(removed).is_none(),
            "project.{removed} must not be exposed in MCP schema"
        );
    }
    assert!(
        props.get("storage").is_some(),
        "project.storage must remain the explicit persistence selector"
    );
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
