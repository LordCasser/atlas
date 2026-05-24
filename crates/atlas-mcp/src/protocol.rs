//! Atlas MCP tool contract types.
//!
//! Wire-level JSON-RPC and stdio framing are handled by the official `rmcp`
//! SDK. This module keeps Atlas' tool schema/result structs so existing tests
//! and tool handlers can remain transport-agnostic.

use serde::Serialize;
use serde_json::Value;

// -------------------------------------------------------------------
// MCP tool contract types
// -------------------------------------------------------------------

/// Tool definition for tools/list.
#[derive(Debug, Serialize, Clone)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: ToolInputSchema,
}

#[derive(Debug, Serialize, Clone)]
pub struct ToolInputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub properties: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

/// Result for tools/list request.
#[derive(Debug, Serialize)]
pub struct ListToolsResult {
    pub tools: Vec<Tool>,
}

/// MCP tool result content types.
#[derive(Debug, Serialize)]
pub struct CallToolResult {
    pub content: Vec<ContentBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
}

impl ContentBlock {
    pub fn text(content: impl Into<String>) -> Self {
        ContentBlock::Text {
            text: content.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tool_serializes_mcp_input_schema_name() {
        let tool = Tool {
            name: "example".into(),
            description: "example tool".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(json!({})),
                required: None,
            },
        };

        let value = serde_json::to_value(tool).unwrap();
        assert!(value.get("inputSchema").is_some());
        assert!(value.get("input_schema").is_none());
    }
}
