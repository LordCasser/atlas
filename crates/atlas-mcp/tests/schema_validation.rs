//! Schema validation tests — catch MCP tool schema drift.
//!
//! These tests verify that critical parameters remain present in the
//! tool input schemas returned by `make_all_tools()`. If you add or
//! remove a parameter intentionally, update the corresponding test here.
//!
//! Run with: `cargo test -p atlas-mcp -- schema`

use atlas_mcp::make_all_tools;

#[test]
fn schema_index_tool_has_analysis_parameter() {
    let tools = make_all_tools();
    let index_tool = tools
        .iter()
        .find(|t| t.name == "index")
        .expect("index tool must exist");

    // Verify analysis parameter exists in the tool's inputSchema
    let props = index_tool
        .input_schema
        .properties
        .as_ref()
        .expect("index tool must have properties in its schema");
    let analysis = props
        .get("analysis")
        .expect("index tool must have 'analysis' parameter");

    // Verify it's not documented as manifest-only
    let desc = analysis
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert!(
        desc.to_lowercase().contains("structural") || desc.to_lowercase().contains("full"),
        "analysis parameter description must mention structural/full modes, got: {desc:?}",
    );

    // Verify the enum values cover all three modes
    let enum_vals = analysis.get("enum").and_then(|v| v.as_array());
    if let Some(vals) = enum_vals {
        let vals: Vec<&str> = vals.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            vals.contains(&"manifest"),
            "analysis enum must contain 'manifest'"
        );
        assert!(
            vals.contains(&"structural"),
            "analysis enum must contain 'structural'"
        );
        assert!(vals.contains(&"full"), "analysis enum must contain 'full'");
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
