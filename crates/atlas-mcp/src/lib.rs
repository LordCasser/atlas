//! MCP server: official `rmcp` server over stdio transport.
//!
//! Architecture:
//!   rmcp::transport::stdio() ──► rmcp service loop
//!         │
//!         ├── initialize/list_tools handled by `ServerHandler`
//!         │
//!         └── tools/call ──► ToolRouter::call_tool()

use std::sync::Arc;
use std::sync::Mutex;

use atlas_engine::Store;
use atlas_engine::Workspace;
use rmcp::ServerHandler;
use rmcp::ServiceExt;
use rmcp::model as rmcp_model;
use rmcp::service::RequestContext;

use self::tools::ToolRouter;

pub mod protocol;
pub mod tools;

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

    /// Async serve loop driven by the official `rmcp` stdio transport.
    async fn serve_async(self) -> anyhow::Result<()> {
        let store = self.store;
        let project_root = self.workspace.root().to_path_buf();
        let service = AtlasMcpService::new(store, project_root);

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

    fn lock_router(&self) -> Result<std::sync::MutexGuard<'_, ToolRouter>, rmcp::ErrorData> {
        self.router
            .lock()
            .map_err(|_| rmcp::ErrorData::internal_error("Atlas MCP router lock poisoned", None))
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
        _context: RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<rmcp_model::CallToolResult, rmcp::ErrorData>> + Send + '_ {
        let start = std::time::Instant::now();
        let tool_name = request.name.to_string();
        let result = self.lock_router().and_then(|mut router| {
            if ToolRouter::tool_requires_graph(&tool_name) {
                router.ensure_graph_initialized().map_err(|err| {
                    rmcp::ErrorData::internal_error(
                        format!("Failed to initialize graph snapshot: {err:#}"),
                        None,
                    )
                })?;
                router.maybe_refresh_graph().map_err(|err| {
                    rmcp::ErrorData::internal_error(
                        format!("Failed to refresh graph snapshot: {err:#}"),
                        None,
                    )
                })?;
            }

            let args = request
                .arguments
                .map(serde_json::Value::Object)
                .unwrap_or(serde_json::Value::Null);
            let tool_result = router.call_tool(&tool_name, &args);
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
        });
        std::future::ready(result)
    }
}
