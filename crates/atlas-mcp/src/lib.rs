//! MCP server: official `rmcp` server over stdio transport.
//!
//! Architecture:
//!   rmcp::transport::stdio() ──► rmcp service loop
//!         │
//!         ├── initialize/list_tools handled by `ServerHandler`
//!         │
//!         └── tools/call ──► ToolRouter::call_tool()
//!
//! Progress notifications for long-running operations (e.g. `index`) follow
//! the MCP spec: a progressToken from `_meta` triggers `notifications/progress`
//! updates through the `peer` transport. See https://modelcontextprotocol.io/specification/2025-11-25/basic/utilities/progress.

use std::sync::Arc;
use std::sync::Mutex;

use atlas_engine::Store;
use atlas_engine::Workspace;
use rmcp::ServerHandler;
use rmcp::ServiceExt;
use rmcp::model as rmcp_model;
use rmcp::model::RequestParamsMeta;
use rmcp::service::RequestContext;

use self::tools::ToolRouter;

pub mod protocol;
pub mod task_manager;
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
        context: RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<rmcp_model::CallToolResult, rmcp::ErrorData>> + Send + '_ {
        let start = std::time::Instant::now();
        let tool_name = request.name.to_string();
        let progress_token = request.progress_token();
        let has_progress_token = progress_token.is_some();

        async move {
            // ── For long-running tools with progress token, set up progress channel ─
            let _progress_task = if matches!(tool_name.as_str(), "index" | "project" | "search")
                && has_progress_token
            {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<tools::ProgressReport>();
                let token = progress_token.unwrap();
                let peer = context.peer.clone();

                // Store the sender on the router so handle_index can use it.
                {
                    let mut router = self.router.lock().map_err(|_| {
                        rmcp::ErrorData::internal_error("Atlas MCP router lock poisoned", None)
                    })?;
                    router.progress_sender = Some(tx);
                }

                // Spawn a task that forwards progress reports to MCP notifications.
                Some(tokio::spawn(async move {
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
                }))
            } else {
                None
            };

            let mut args = request
                .arguments
                .map(serde_json::Value::Object)
                .unwrap_or(serde_json::Value::Null);
            if should_auto_background_without_progress(&tool_name, &args, has_progress_token) {
                ensure_object_bool(&mut args, "background", true);
                ensure_object_bool(&mut args, "_auto_background", true);
            }

            // ── Standard tool dispatch ────────────────────────────────────
            let result = self.lock_router().and_then(|mut router| {
                if ToolRouter::tool_call_requires_graph(&tool_name, &args) {
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

            // ── Clean up progress sender ──────────────────────────────────
            {
                let mut router = self.router.lock().map_err(|_| {
                    rmcp::ErrorData::internal_error("Atlas MCP router lock poisoned", None)
                })?;
                router.progress_sender = None;
            }

            // Wait for the progress notification task to finish (receiver dropped).
            if let Some(handle) = _progress_task {
                let _ = handle.await;
            }

            result
        }
    }
}

fn should_auto_background_without_progress(
    tool_name: &str,
    args: &serde_json::Value,
    has_progress_token: bool,
) -> bool {
    if has_progress_token {
        return false;
    }
    match tool_name {
        "index" => true,
        "project" => args
            .get("scan_files")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        _ => false,
    }
}

fn ensure_object_bool(args: &mut serde_json::Value, key: &str, value: bool) {
    if !args.is_object() {
        *args = serde_json::Value::Object(Default::default());
    }
    if let Some(obj) = args.as_object_mut() {
        obj.insert(key.to_string(), serde_json::Value::Bool(value));
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn auto_background_policy_protects_no_progress_clients() {
        assert!(super::should_auto_background_without_progress(
            "index",
            &json!({}),
            false
        ));
        assert!(!super::should_auto_background_without_progress(
            "index",
            &json!({}),
            true
        ));
        assert!(super::should_auto_background_without_progress(
            "project",
            &json!({ "scan_files": true }),
            false
        ));
        assert!(!super::should_auto_background_without_progress(
            "project",
            &json!({}),
            false
        ));
        assert!(!super::should_auto_background_without_progress(
            "search",
            &json!({}),
            false
        ));
    }
}
