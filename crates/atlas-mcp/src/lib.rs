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

use atlas_context::ContextBuilder;
use atlas_engine::Store;
use atlas_engine::GraphEngine;
use atlas_search::SearchEngine;
use atlas_engine::Workspace;

use self::protocol::{Response, ServerCapabilities, ServerInfo, ToolsCapability};
use self::tools::ToolRouter;

pub mod protocol;
pub mod tools;
pub mod transport;

// Re-export for integration tests and diagnostics
pub use protocol::Tool;
pub use tools::make_all_tools;

/// The MCP server orchestrator.
pub struct McpServer {
    store: Arc<Store>,
    workspace: Workspace,
}

impl McpServer {
    /// Create a new MCP server backed by the given store and workspace.
    pub fn new(store: Arc<Store>, workspace: Workspace) -> Self {
        Self { store, workspace }
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

        // Build the tool router with initial graph engine.
        // Graph snapshot is loaded once at startup and remains stable
        // for the server lifetime. Use `atlas_status` or server restart
        // to refresh the graph after sync/index operations.
        let store = self.store;
        let search = SearchEngine::new(
            Arc::clone(&store),
            Arc::new(GraphEngine::from_store(&store, 0.3)?),
        );
        let context = ContextBuilder::new(
            Arc::clone(&store),
            Arc::new(GraphEngine::from_store(&store, 0.3)?),
        );
        let mut router = ToolRouter::new(
            Arc::clone(&store),
            search,
            context,
            self.workspace.root().to_path_buf(),
        );

        loop {
            let request = match transport::read_request(&mut reader).await? {
                Some(req) => req,
                None => break, // EOF
            };

            // Check if DB has been updated since last graph build.
            // If so, refresh graph/snapshot to keep trace and graph tools
            // consistent within the same MCP session.
            router.maybe_refresh_graph()?;

            let response = {
                let start = std::time::Instant::now();
                let outcome = Self::dispatch(&request, &router);
                let duration_ms = start.elapsed().as_millis() as u64;
                let ok = outcome.response.error.is_none() && !outcome.tool_error;
                let _span = tracing::info_span!(
                    "mcp_request",
                    method = %request.method,
                    id = ?request.id,
                    tool_name = outcome.tool_name.as_deref().unwrap_or(""),
                    tool_error = outcome.tool_error,
                    duration_ms = duration_ms,
                    ok = ok,
                );
                tracing::info!(parent: &_span, "request handled");
                outcome.response
            };
            transport::write_response(&mut stdout, &response).await?;
        }

        Ok(())
    }
}

/// Internal result of `dispatch` carrying tool-level error state in
/// addition to the JSON-RPC response, so the `serve_async` tracing span
/// can correctly report `ok = false` for MCP tool errors (which live in
/// `CallToolResult.is_error`, not `Response.error`).
struct DispatchOutcome {
    response: Response,
    tool_name: Option<String>,
    tool_error: bool,
}

impl McpServer {
    /// Dispatch a single JSON-RPC request.
    fn dispatch(request: &protocol::Request, router: &ToolRouter) -> DispatchOutcome {
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
                }))
                .ok();

                DispatchOutcome {
                    response: Response {
                        jsonrpc: protocol::JSONRPC_VERSION,
                        id: request.id.clone(),
                        result,
                        error: None,
                    },
                    tool_name: None,
                    tool_error: false,
                }
            }

            "tools/list" => {
                let list_result = router.list_tools();
                let result = serde_json::to_value(list_result).ok();
                DispatchOutcome {
                    response: Response {
                        jsonrpc: protocol::JSONRPC_VERSION,
                        id: request.id.clone(),
                        result,
                        error: None,
                    },
                    tool_name: None,
                    tool_error: false,
                }
            }

            "tools/call" => {
                let params = match request.params.as_ref() {
                    Some(p) => p,
                    None => {
                        return DispatchOutcome {
                            response: Response::error(
                                request.id.clone(),
                                protocol::INVALID_PARAMS,
                                "Missing params".into(),
                            ),
                            tool_name: None,
                            tool_error: true,
                        };
                    }
                };

                let name = match params.get("name").and_then(|v| v.as_str()) {
                    Some(n) => n,
                    None => {
                        return DispatchOutcome {
                            response: Response::error(
                                request.id.clone(),
                                protocol::INVALID_PARAMS,
                                "Missing tool name".into(),
                            ),
                            tool_name: None,
                            tool_error: true,
                        };
                    }
                };

                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let tool_result = router.call_tool(name, &args);
                let tool_error = tool_result.is_error.unwrap_or(false);
                let result = serde_json::to_value(tool_result).ok();

                DispatchOutcome {
                    response: Response {
                        jsonrpc: protocol::JSONRPC_VERSION,
                        id: request.id.clone(),
                        result,
                        error: None,
                    },
                    tool_name: Some(name.to_string()),
                    tool_error,
                }
            }

            _ => DispatchOutcome {
                response: Response::error(
                    request.id.clone(),
                    protocol::METHOD_NOT_FOUND,
                    format!("Method not found: {}", request.method),
                ),
                tool_name: None,
                tool_error: true,
            },
        }
    }
}
