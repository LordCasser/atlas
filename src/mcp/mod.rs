//! MCP server: JSON-RPC over stdio, tool definitions.

pub mod transport;
pub mod tools;
pub mod protocol;

/// MCP server entry point.
pub struct McpServer {
    _placeholder: (),
}

impl McpServer {
    pub fn new() -> Self {
        Self { _placeholder: () }
    }
}
