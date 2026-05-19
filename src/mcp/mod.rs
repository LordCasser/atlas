//! MCP server: JSON-RPC 2.0 over stdio transport.
//!
//! Architecture:
//!   transport::read_request()  ──► Request
//!         |
//!         v
//!   dispatch(): initialize / tools/list / tools/call
//!         |
//!         v
//!   ToolRouter::call_tool()  ──► CallToolResult
//!         |
//!         v
//!   transport::write_response() ──► stdio Content-Length framing

use std::sync::Arc;

use crate::db::Store;
use crate::graph::GraphEngine;
use crate::search::SearchEngine;
use crate::context::ContextBuilder;

use self::protocol::{Response, ServerCapabilities, ServerInfo, ToolsCapability};
use self::tools::ToolRouter;

pub mod transport;
pub mod tools;
pub mod protocol;

// Re-export for integration tests and diagnostics
pub use tools::make_all_tools;
pub use protocol::Tool;

/// The MCP server orchestrator.
pub struct McpServer {
    store: Arc<Store>,
}

impl McpServer {
    /// Create a new MCP server backed by the given store.
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Start the MCP server loop (blocking).
    ///
    /// Initializes a tokio runtime and runs the async serve loop.
    pub fn serve(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        rt.block_on(self.serve_async())
    }

    /// Async serve loop: read JSON-RPC requests from stdin, dispatch, write to stdout.
    async fn serve_async(self) -> anyhow::Result<()> {
        use tokio::io::BufReader;

        let stdin = tokio::io::stdin();
        let mut reader = BufReader::new(stdin);
        let mut stdout = tokio::io::stdout();

        // Build the tool router with fresh graph engine per request.
        // This ensures that after sync/index operations, MCP tools see the latest data.
        let store = self.store;
        let search = SearchEngine::new(Arc::clone(&store), Arc::new(
            GraphEngine::from_store(&store, 0.3)?,
        ));
        let context = ContextBuilder::new(Arc::clone(&store), Arc::new(
            GraphEngine::from_store(&store, 0.3)?,
        ));
        // graph_fn rebuilds from store on every call to avoid staleness
        let store_for_graph = Arc::clone(&store);
        let graph_fn = move || GraphEngine::from_store(&store_for_graph, 0.3)
            .expect("Failed to reload graph snapshot from store");
        let router = ToolRouter::new(Arc::clone(&store), search, context, graph_fn);

        loop {
            let request = match transport::read_request(&mut reader).await? {
                Some(req) => req,
                None => break, // EOF
            };

            let response = Self::dispatch(&request, &router);
            transport::write_response(&mut stdout, &response).await?;
        }

        Ok(())
    }

    /// Dispatch a single JSON-RPC request.
    fn dispatch(request: &protocol::Request, router: &ToolRouter) -> Response {
        match request.method.as_str() {
            "initialize" => {
                let result = serde_json::to_value(serde_json::json!({
                    "protocolVersion": "0.1.0",
                    "capabilities": ServerCapabilities {
                        tools: Some(ToolsCapability { list_changed: false }),
                    },
                    "serverInfo": ServerInfo {
                        name: "atlas-mcp".into(),
                        version: env!("CARGO_PKG_VERSION").into(),
                    },
                })).ok();

                Response {
                    jsonrpc: protocol::JSONRPC_VERSION,
                    id: request.id.clone(),
                    result,
                    error: None,
                }
            }

            "tools/list" => {
                let list_result = router.list_tools();
                let result = serde_json::to_value(list_result).ok();
                Response {
                    jsonrpc: protocol::JSONRPC_VERSION,
                    id: request.id.clone(),
                    result,
                    error: None,
                }
            }

            "tools/call" => {
                let params = match request.params.as_ref() {
                    Some(p) => p,
                    None => return Response::error(
                        request.id.clone(),
                        protocol::INVALID_PARAMS,
                        "Missing params".into(),
                    ),
                };

                let name = match params.get("name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => return Response::error(
                        request.id.clone(),
                        protocol::INVALID_PARAMS,
                        "Missing tool name".into(),
                    ),
                };

                let args = params.get("arguments").cloned().unwrap_or(serde_json::Value::Null);
                let tool_result = router.call_tool(name, &args);
                let result = serde_json::to_value(tool_result).ok();

                Response {
                    jsonrpc: protocol::JSONRPC_VERSION,
                    id: request.id.clone(),
                    result,
                    error: None,
                }
            }

            _ => Response::error(
                request.id.clone(),
                protocol::METHOD_NOT_FOUND,
                format!("Method not found: {}", request.method),
            ),
        }
    }
}
