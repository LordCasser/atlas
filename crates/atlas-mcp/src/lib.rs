//! MCP server: official `rmcp` server over stdio transport.
//!
//! Architecture:
//!   rmcp::transport::stdio() ──► rmcp service loop
//!         │
//!         ├── initialize/list_tools handled by `ServerHandler`
//!         └── tools/call ──► ToolRouter::call_tool()
//!
//! Progress notifications follow the MCP spec: a progressToken from `_meta`
//! triggers `notifications/progress` updates through the `peer` transport.
//! See https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/progress.

use std::sync::Arc;

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

const MAX_CONCURRENT_TOOL_CALLS: usize = 4;

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

/// `rmcp` service adapter around Atlas' tool router.
struct AtlasMcpService {
    router: Arc<ToolRouter>,
    blocking_gate: Arc<tokio::sync::Semaphore>,
}

impl AtlasMcpService {
    fn new(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        Self {
            router: Arc::new(ToolRouter::new_empty(store, project_root)),
            blocking_gate: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TOOL_CALLS)),
        }
    }

    fn new_unopened() -> Self {
        Self {
            router: Arc::new(ToolRouter::new_unopened()),
            blocking_gate: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TOOL_CALLS)),
        }
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
        let tools = self
            .router
            .list_tools()
            .tools
            .into_iter()
            .map(Self::to_rmcp_tool)
            .collect();
        let result = Ok(rmcp_model::ListToolsResult::with_all_items(tools));
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
        let router = Arc::clone(&self.router);
        let blocking_gate = Arc::clone(&self.blocking_gate);

        let args = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);

        async move {
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

            let blocking_ctx = ctx.clone();
            let blocking_tool_name = tool_name.clone();
            let blocking_permit = blocking_gate.acquire_owned().await.map_err(|error| {
                rmcp::ErrorData::internal_error(
                    "Atlas tool gate closed",
                    Some(serde_json::json!({ "detail": error.to_string() })),
                )
            })?;
            let tool_result = tokio::task::spawn_blocking(move || {
                let _blocking_permit = blocking_permit;
                router.call_tool(&blocking_ctx, &blocking_tool_name, &args)
            })
            .await
            .map_err(|error| {
                rmcp::ErrorData::internal_error(
                    "Atlas tool worker failed",
                    Some(serde_json::json!({ "detail": error.to_string() })),
                )
            })?;
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
            let result = Ok(Self::to_rmcp_result(tool_result));

            // Close the request-scoped sender before awaiting the forwarder.
            // Otherwise a sync request carrying a progress token waits forever
            // for a receiver that cannot observe channel closure.
            drop(ctx);
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

    #[test]
    fn tool_calls_have_a_fixed_blocking_concurrency_bound() {
        let service = super::AtlasMcpService::new_unopened();
        assert_eq!(
            service.blocking_gate.available_permits(),
            super::MAX_CONCURRENT_TOOL_CALLS
        );
    }

    #[test]
    fn server_new_is_constructable() {
        // Verify the server struct compiles without Mutex
    }
}
