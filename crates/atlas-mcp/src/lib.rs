//! MCP server: official `rmcp` server over stdio transport.
//!
//! Architecture:
//!   rmcp::transport::stdio() ──► rmcp service loop
//!         │
//!         ├── initialize/list_tools handled by `ServerHandler`
//!         │
//!         └── tools/call ──► ToolRouter::call_tool()
//!
//! Progress notifications follow the MCP spec: a progressToken from `_meta`
//! triggers `notifications/progress` updates through the `peer` transport.
//! See https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/progress.

use std::sync::{Arc, Mutex, TryLockError};

use atlas_engine::Store;
use atlas_engine::Workspace;
use rmcp::ServerHandler;
use rmcp::ServiceExt;
use rmcp::model as rmcp_model;
use rmcp::model::RequestParamsMeta;
use rmcp::service::RequestContext;

use self::tools::ToolRouter;

pub mod protocol;
pub mod tools;

// Re-export for integration tests and diagnostics
pub use protocol::Tool;
pub use tools::make_all_tools;

/// The MCP server orchestrator.
pub struct McpServer {
    initial_project: Option<(Arc<Store>, Workspace)>,
}

impl McpServer {
    /// Create a new MCP server backed by the given store and workspace.
    pub fn new(store: Arc<Store>, workspace: Workspace) -> Self {
        Self {
            initial_project: Some((store, workspace)),
        }
    }

    /// Create a new MCP server without an active project.
    pub fn new_unopened() -> Self {
        Self {
            initial_project: None,
        }
    }

    /// Start the MCP server loop (blocking).
    ///
    /// Initializes a tokio runtime and runs the async serve loop.
    ///
    /// Uses a multi-thread runtime so that the progress-forwarder task
    /// (`tokio::spawn` in `call_tool`) can run on a separate worker while
    /// the primary tool dispatch holds the router mutex for synchronous
    /// CPU/IO work.  Two workers are sufficient: one for the rmcp
    /// serve loop and progress forwarding.
    pub fn serve(self) -> anyhow::Result<()> {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        rt.block_on(self.serve_async())
    }

    /// Async serve loop driven by the official `rmcp` stdio transport.
    async fn serve_async(self) -> anyhow::Result<()> {
        let service = match self.initial_project {
            Some((store, workspace)) => AtlasMcpService::new(store, workspace.root().to_path_buf()),
            None => AtlasMcpService::new_unopened(),
        };

        let running = service
            .serve(rmcp::transport::stdio())
            .await
            .map_err(|err| anyhow::anyhow!("MCP server initialize failed: {err}"))?;
        running
            .waiting()
            .await
            .map_err(|err| anyhow::anyhow!("MCP server task failed: {err}"))?;
        Ok(())
    }
}

/// `rmcp` service adapter around Atlas' existing tool router.
struct AtlasMcpService {
    router: Mutex<ToolRouter>,
}

impl AtlasMcpService {
    fn new(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        Self {
            router: Mutex::new(ToolRouter::new_empty(store, project_root)),
        }
    }

    fn new_unopened() -> Self {
        Self {
            router: Mutex::new(ToolRouter::new_unopened()),
        }
    }

    fn lock_router(&self) -> Result<std::sync::MutexGuard<'_, ToolRouter>, rmcp::ErrorData> {
        Ok(self.router.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn to_rmcp_tool(tool: protocol::Tool) -> rmcp_model::Tool {
        let mut schema = rmcp_model::JsonObject::new();
        schema.insert(
            "type".into(),
            serde_json::Value::String(tool.input_schema.schema_type),
        );
        if let Some(properties) = tool.input_schema.properties {
            schema.insert("properties".into(), properties);
        }
        if let Some(required) = tool.input_schema.required {
            schema.insert(
                "required".into(),
                serde_json::Value::Array(
                    required
                        .into_iter()
                        .map(serde_json::Value::String)
                        .collect(),
                ),
            );
        }

        rmcp_model::Tool::new_with_raw(tool.name, Some(tool.description.into()), schema)
    }

    fn to_rmcp_result(result: protocol::CallToolResult) -> rmcp_model::CallToolResult {
        let content = result
            .content
            .into_iter()
            .map(|block| match block {
                protocol::ContentBlock::Text { text } => rmcp_model::Content::text(text),
            })
            .collect();

        if result.is_error.unwrap_or(false) {
            rmcp_model::CallToolResult::error(content)
        } else {
            rmcp_model::CallToolResult::success(content)
        }
    }

    fn router_busy_result(tool_name: &str) -> rmcp_model::CallToolResult {
        let body = serde_json::json!({
            "partial_result": true,
            "retry_after_ms": 1000,
            "tool": tool_name,
            "analysis": {
                "state": "server_busy",
                "next_action": "retry",
                "scope": "server",
                "summary": "Atlas is handling another tool call; retry shortly instead of waiting for the MCP request to time out."
            }
        });
        rmcp_model::CallToolResult::success(vec![rmcp_model::Content::text(
            serde_json::to_string_pretty(&body).unwrap_or_else(|e| e.to_string()),
        )])
    }
}

impl ServerHandler for AtlasMcpService {
    fn get_info(&self) -> rmcp_model::ServerInfo {
        rmcp_model::ServerInfo::new(
            rmcp_model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        )
        .with_server_info(rmcp_model::Implementation::new(
            "atlas-mcp",
            env!("CARGO_PKG_VERSION"),
        ))
    }

    fn list_tools(
        &self,
        _request: Option<rmcp_model::PaginatedRequestParams>,
        _context: RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<rmcp_model::ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        let result = self.lock_router().map(|router| {
            let tools = router
                .list_tools()
                .tools
                .into_iter()
                .map(Self::to_rmcp_tool)
                .collect();
            rmcp_model::ListToolsResult::with_all_items(tools)
        });
        std::future::ready(result)
    }

    fn call_tool(
        &self,
        request: rmcp_model::CallToolRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<rmcp_model::CallToolResult, rmcp::ErrorData>> + Send + '_ {
        let start = std::time::Instant::now();
        let tool_name = request.name.to_string();
        let progress_token = request.progress_token();
        let has_progress_token = progress_token.is_some();

        async move {
            // ── Build request-scoped context (replaces global progress_sender) ──
            // For long-running tools with a progress token, create a channel and
            // spawn a forwarder that converts ProgressReport → MCP notifications.
            let (ctx, _progress_task) = if matches!(
                tool_name.as_str(),
                "project" | "search" | "symbol" | "trace"
            ) && has_progress_token
            {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<tools::ProgressReport>();
                let token = progress_token.unwrap();
                let peer = context.peer.clone();

                let ctx = tools::ToolCallContext::with_progress_sender(tx);

                let forwarder = tokio::spawn(async move {
                    while let Some((progress, total, message)) = rx.recv().await {
                        let mut params =
                            rmcp_model::ProgressNotificationParam::new(token.clone(), progress);
                        if let Some(t) = total {
                            params = params.with_total(t);
                        }
                        if let Some(m) = message {
                            params = params.with_message(m);
                        }
                        let _ = peer.notify_progress(params).await;
                    }
                });

                (ctx, Some(forwarder))
            } else {
                (tools::ToolCallContext::empty(), None)
            };

            let args = request
                .arguments
                .map(serde_json::Value::Object)
                .unwrap_or(serde_json::Value::Null);

            // ── Standard tool dispatch ────────────────────────────────────
            // Resource preparation (graph init / refresh) is now handled
            // inside call_tool() based on the ToolContract.  The server
            // layer only locks the router and delegates. If another request is
            // already running, return a retryable response instead of blocking
            // until the MCP client times out.
            let result = match self.router.try_lock() {
                Ok(mut router) => {
                    let tool_result = router.call_tool(&ctx, &tool_name, &args);
                    let tool_error = tool_result.is_error.unwrap_or(false);
                    let duration_ms = start.elapsed().as_millis() as u64;
                    let _span = tracing::info_span!(
                        "mcp_request",
                        method = "tools/call",
                        tool_name = %tool_name,
                        tool_error = tool_error,
                        duration_ms = duration_ms,
                        ok = !tool_error,
                    );
                    tracing::info!(parent: &_span, "request handled");
                    Ok(Self::to_rmcp_result(tool_result))
                }
                Err(TryLockError::Poisoned(poisoned)) => {
                    let mut router = poisoned.into_inner();
                    let tool_result = router.call_tool(&ctx, &tool_name, &args);
                    Ok(Self::to_rmcp_result(tool_result))
                }
                Err(TryLockError::WouldBlock) => {
                    tracing::info!(
                        method = "tools/call",
                        tool_name = %tool_name,
                        duration_ms = start.elapsed().as_millis() as u64,
                        "router busy; returning retryable response"
                    );
                    Ok(Self::router_busy_result(&tool_name))
                }
            };

            // Wait for the progress notification task to finish (receiver dropped).
            if let Some(handle) = _progress_task {
                let _ = handle.await;
            }

            result
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unopened_server_can_be_constructed() {
        let _server = super::McpServer::new_unopened();
    }
}
