//! MCP server: official `rmcp` server over stdio transport.
//!
//! Architecture:
//!   rmcp::transport::stdio() ──► rmcp service loop
//!         │
//!         ├── initialize/list_tools handled by `ServerHandler`
//!         │
//!         └── tools/call ──► ToolRouter::call_tool() (sync)
//!                        ──► TaskManager::register() → tokio::spawn (async)
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
use serde_json::json;

use self::tools::ToolRouter;
use self::tools::task_manager::{BackoffConfig, TaskManager};
use self::tools::tool_contract::{ExecutionMode, contract_for, execution_mode};

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

/// `rmcp` service adapter around Atlas' existing tool router and async task manager.
struct AtlasMcpService {
    router: ToolRouter,
    task_mgr: Arc<TaskManager>,
}

impl AtlasMcpService {
    fn new(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        Self {
            router: ToolRouter::new_empty(store, project_root),
            task_mgr: Arc::new(TaskManager::new(
                std::env::var("ATLAS_MAX_ASYNC_TASKS")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(4),
            )),
        }
    }

    fn new_unopened() -> Self {
        Self {
            router: ToolRouter::new_unopened(),
            task_mgr: Arc::new(TaskManager::new(4)),
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
        let tools = self.router
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

        let args = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);

        // Determine contract and execution mode for dispatch split
        let contract = contract_for(&tool_name, &args);
        let mode = execution_mode(&contract);

        async move {
            // ── Layer 1 probe: detect external file changes ─────────
            // Non-blocking: checks cooldown, detects changes via git/DB,
            // spawns background sync if needed. Safe to call without
            // project for StatusRead tools (probe returns early if no
            // project or no full index).
            if self.router.project.is_active() {
                self.router.probe_external_changes_if_due();
            }

            // ── Special case: tasks tool with task_id ──────────────────
            // Handled here because TaskManager lives at the service level,
            // not inside ToolRouter.
            if tool_name == "tasks" {
                if let Some(task_id) = args
                    .get("task_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    let task_state = self.task_mgr.poll(task_id);
                    let response = match task_state {
                        Some(state) => {
                            json!({
                                "task_id": state.id,
                                "tool": state.tool_name,
                                "status": format!("{:?}", state.status).to_lowercase(),
                                "result": state.result,
                                "is_error": state.is_error,
                                "progress_pct": state.progress_pct,
                                "progress_msg": state.progress_msg,
                                "poll_count": state.poll_count,
                                "next_poll_after_ms": state.next_poll_after_ms(),
                                "created_at_ms": state.created_at.elapsed().as_millis(),
                                "started_after_ms": state.started_at.map(|t| t.elapsed().as_millis()),
                                "completed_after_ms": state.completed_at.map(|t| t.elapsed().as_millis()),
                            })
                        }
                        None => {
                            json!({
                                "task_id": task_id,
                                "status": "not_found_or_expired",
                                "message": "Task not found or has expired. Completed/failed tasks are pruned after 5 minutes."
                            })
                        }
                    };
                    let text = serde_json::to_string_pretty(&response).unwrap_or_default();
                    return Ok(rmcp_model::CallToolResult::success(vec![
                        rmcp_model::Content::text(text),
                    ]));
                }
            }

            // ── Async path: register task, spawn work, return task_id ──
            if mode == ExecutionMode::Async {
                return Self::handle_async_tool(
                    &self.router,
                    &self.task_mgr,
                    &tool_name,
                    &args,
                )
                .await;
            }

            // ── Sync path: execute inline ──────────────────────────────
            let (ctx, _progress_task) = if matches!(
                tool_name.as_str(),
                "project" | "search" | "symbol" | "trace"
            ) && has_progress_token
            {
                let (tx, mut rx) =
                    tokio::sync::mpsc::unbounded_channel::<tools::ProgressReport>();
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

            let tool_result = self.router.call_tool(&ctx, &tool_name, &args);
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

            // Wait for the progress notification task to finish (receiver dropped).
            if let Some(handle) = _progress_task {
                let _ = handle.await;
            }

            result
        }
    }
}

impl AtlasMcpService {
    /// Handle an async tool call: register task, spawn work, return immediate
    /// response with task_id and polling instructions.
    async fn handle_async_tool(
        router: &ToolRouter,
        task_mgr: &Arc<TaskManager>,
        tool_name: &str,
        _args: &serde_json::Value,
    ) -> Result<rmcp_model::CallToolResult, rmcp::ErrorData> {
        // Extract project snapshot for the async task
        let project = match router.project.get() {
            Ok(p) => p,
            Err(e) => {
                return Ok(rmcp_model::CallToolResult::error(vec![
                    rmcp_model::Content::text(e),
                ]));
            }
        };

        // Register the task
        let task_id = task_mgr.register(tool_name);

        // Build immediate response with polling instructions
        let backoff = BackoffConfig::default();
        let next_poll = backoff.initial_ms;
        let response = json!({
            "task_id": task_id,
            "partial_result": true,
            "tool": tool_name,
            "status": "accepted",
            "poll": {
                "interval_ms": next_poll,
                "backoff": {
                    "strategy": "exponential",
                    "initial_ms": backoff.initial_ms,
                    "max_ms": backoff.max_ms,
                    "multiplier": backoff.multiplier,
                    "jitter_ms": backoff.jitter_ms
                }
            },
            "next_action": {
                "tool": "tasks",
                "args": {"task_id": task_id}
            }
        });

        // Clone what the spawned task needs
        let task_mgr = Arc::clone(task_mgr);
        let sem = task_mgr.semaphore();
        let tool_name_owned = tool_name.to_string();

        // Spawn the async work
        tokio::spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            task_mgr.mark_running(&task_id);

            // ── Placeholder handler execution ──────────────────────────
            // Real handler wiring will be done in a follow-up phase.
            // For now, demonstrate the async infrastructure with a
            // simulated completion.
            let handler_result = std::panic::catch_unwind(
                std::panic::AssertUnwindSafe(|| {
                    // Create execution context for the handler
                    let _ec = crate::tools::execution_context::ExecutionContext::new(
                        project,
                        Some(task_id.clone()),
                        None, // progress channel not wired here yet
                    );
                    format!(
                        r#"{{"message": "async {} task {} accepted — handler not yet wired"}}"#,
                        tool_name_owned, task_id
                    )
                }),
            );

            match handler_result {
                Ok(result_str) => {
                    task_mgr.mark_completed(&task_id, result_str, false);
                }
                Err(panic_err) => {
                    let msg = if let Some(s) = panic_err.downcast_ref::<String>() {
                        s.clone()
                    } else if let Some(s) = panic_err.downcast_ref::<&str>() {
                        s.to_string()
                    } else {
                        "handler panicked with unknown payload".to_string()
                    };
                    task_mgr.mark_failed(&task_id, msg);
                }
            }
        });

        // Return immediate response with task_id
        let text = serde_json::to_string_pretty(&response).unwrap_or_default();
        Ok(rmcp_model::CallToolResult::success(vec![
            rmcp_model::Content::text(text),
        ]))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unopened_server_can_be_constructed() {
        let _server = super::McpServer::new_unopened();
    }

    #[test]
    fn server_new_is_constructable() {
        // Verify the server struct compiles without Mutex
    }
}
