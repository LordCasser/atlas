//! Atlas MCP tool contract types.
//!
//! Wire-level JSON-RPC and stdio framing are handled by the official `rmcp`
//! SDK. This module keeps Atlas' tool schema/result structs so existing tests
//! and tool handlers can remain transport-agnostic.

use serde::Serialize;
use serde_json::{Map, Value};

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

#[derive(Debug, Serialize, Clone, PartialEq)]
#[serde(transparent)]
pub struct ToolInputSchema {
    schema: Map<String, Value>,
}

impl ToolInputSchema {
    /// Wrap a complete JSON Schema object without filtering root-level keywords.
    pub fn from_object(schema: Map<String, Value>) -> Self {
        Self { schema }
    }

    /// Build the object-shaped input schemas used by Atlas' current tool catalog.
    pub fn object(properties: Value, required: Option<Vec<String>>) -> Self {
        let mut schema = Map::new();
        schema.insert("type".into(), Value::String("object".into()));
        schema.insert("properties".into(), properties);
        if let Some(required) = required {
            schema.insert(
                "required".into(),
                Value::Array(required.into_iter().map(Value::String).collect()),
            );
        }
        Self::from_object(schema)
    }

    /// Borrow the complete JSON Schema object.
    pub fn as_object(&self) -> &Map<String, Value> {
        &self.schema
    }

    /// Consume the wrapper and return the complete JSON Schema object.
    pub fn into_object(self) -> Map<String, Value> {
        self.schema
    }

    /// Read a root-level JSON Schema keyword.
    pub fn get(&self, keyword: &str) -> Option<&Value> {
        self.schema.get(keyword)
    }
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
            input_schema: ToolInputSchema::object(json!({}), None),
        };

        let value = serde_json::to_value(tool).unwrap();
        assert_eq!(
            value["inputSchema"],
            json!({"type": "object", "properties": {}})
        );
        assert!(value.get("input_schema").is_none());
    }

    #[test]
    fn complete_input_schema_object_round_trips_without_projection() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "$defs": {"selector": {"type": "string"}},
            "allOf": [{"$ref": "#/$defs/selector"}],
            "x-atlas-future-keyword": {"enabled": true}
        });
        let schema_object = schema.as_object().expect("schema is an object").clone();
        let input_schema = ToolInputSchema::from_object(schema_object.clone());

        assert_eq!(input_schema.as_object(), &schema_object);
        assert_eq!(serde_json::to_value(input_schema).unwrap(), schema);
    }
}
