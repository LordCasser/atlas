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
use self::tools::task_manager::{BackoffConfig, TaskManager, TaskState, TaskStatus};
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
                return Self::handle_async_tool(&self.router, &self.task_mgr, &tool_name, &args)
                    .await;
            }

            // ── Sync path: execute inline ──────────────────────────────
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
        args: &serde_json::Value,
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
        let task_id_for_response = task_id.clone();

        // Build immediate response with polling instructions
        let backoff = BackoffConfig::default();
        let next_poll = backoff.initial_ms;
        let response = json!({
            "task_id": task_id_for_response,
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
                "args": {"task_id": task_id_for_response}
            }
        });

        // Clone what the spawned task needs
        let task_mgr_for_worker = Arc::clone(task_mgr);
        let sem = task_mgr.semaphore();
        let tool_name_owned = tool_name.to_string();
        let args_owned = args.clone();
        let task_router = ToolRouter::from_active_project(project);

        // Spawn the async work
        tokio::spawn(async move {
            let permit = match sem.acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => {
                    task_mgr_for_worker
                        .mark_failed(&task_id, "async task semaphore closed".to_string());
                    return;
                }
            };
            task_mgr_for_worker.mark_running(&task_id);

            let task_id_for_block = task_id.clone();
            let blocking = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let ctx = tools::ToolCallContext::empty();
                    task_router.call_tool(&ctx, &tool_name_owned, &args_owned)
                }))
            })
            .await;

            match blocking {
                Ok(Ok(tool_result)) => {
                    let is_error = tool_result.is_error.unwrap_or(false);
                    let result_text = tool_result
                        .content
                        .into_iter()
                        .map(|block| match block {
                            protocol::ContentBlock::Text { text } => text,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    task_mgr_for_worker.mark_completed(&task_id, result_text, is_error);
                }
                Ok(Err(panic_err)) => {
                    let msg = panic_message(panic_err);
                    task_mgr_for_worker.mark_failed(&task_id, msg);
                }
                Err(join_err) => {
                    task_mgr_for_worker.mark_failed(
                        &task_id_for_block,
                        format!("async handler task failed: {join_err}"),
                    );
                }
            }
        });

        if let Some(state) =
            wait_for_task_completion(task_mgr, &task_id_for_response, sync_wait_timeout()).await
        {
            return Ok(task_state_to_rmcp_result(state));
        }

        // Return polling contract only when the handler exceeded the sync wait budget.
        let text = serde_json::to_string_pretty(&response).unwrap_or_default();
        Ok(rmcp_model::CallToolResult::success(vec![
            rmcp_model::Content::text(text),
        ]))
    }
}

fn sync_wait_timeout() -> std::time::Duration {
    let millis = std::env::var("ATLAS_MCP_SYNC_WAIT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(25_000);
    std::time::Duration::from_millis(millis)
}

async fn wait_for_task_completion(
    task_mgr: &Arc<TaskManager>,
    task_id: &str,
    timeout: std::time::Duration,
) -> Option<TaskState> {
    let start = std::time::Instant::now();
    loop {
        if let Some(state) = task_mgr
            .list_all()
            .into_iter()
            .find(|state| state.id == task_id)
        {
            if matches!(
                state.status,
                TaskStatus::Completed | TaskStatus::Failed { .. }
            ) {
                return Some(state);
            }
        }

        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return None;
        }
        let remaining = timeout.saturating_sub(elapsed);
        tokio::time::sleep(std::cmp::min(
            remaining,
            std::time::Duration::from_millis(25),
        ))
        .await;
    }
}

fn task_state_to_rmcp_result(state: TaskState) -> rmcp_model::CallToolResult {
    let is_error = match state.status {
        TaskStatus::Failed { .. } => true,
        _ => state.is_error.unwrap_or(false),
    };
    let text = state.result.unwrap_or_else(|| {
        json!({
            "task_id": state.id,
            "status": format!("{:?}", state.status).to_lowercase(),
            "message": "task finished without a result payload"
        })
        .to_string()
    });
    let content = vec![rmcp_model::Content::text(text)];
    if is_error {
        rmcp_model::CallToolResult::error(content)
    } else {
        rmcp_model::CallToolResult::success(content)
    }
}

fn panic_message(panic_err: Box<dyn std::any::Any + Send>) -> String {
    if let Some(s) = panic_err.downcast_ref::<String>() {
        s.clone()
    } else if let Some(s) = panic_err.downcast_ref::<&str>() {
        s.to_string()
    } else {
        "handler panicked with unknown payload".to_string()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use atlas_engine::Store;
    use serde_json::json;

    use super::tools::task_manager::TaskStatus;

    #[test]
    fn unopened_server_can_be_constructed() {
        let _server = super::McpServer::new_unopened();
    }

    #[test]
    fn server_new_is_constructable() {
        // Verify the server struct compiles without Mutex
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_tool_returns_immediate_result_when_completed_within_budget() {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let service = super::AtlasMcpService::new(store, temp.path().to_path_buf());

        let args = json!({"file_path": "missing.c"});
        let accepted = super::AtlasMcpService::handle_async_tool(
            &service.router,
            &service.task_mgr,
            "file_dependencies",
            &args,
        )
        .await
        .unwrap();
        assert!(accepted.is_error.unwrap_or(false));
        let result = accepted
            .content
            .into_iter()
            .map(|content| format!("{content:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(result.contains("File not found: missing.c"), "{result}");
        assert!(!result.contains("handler not yet wired"), "{result}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pending_task_can_timeout_to_polling_contract() {
        let mgr = Arc::new(super::TaskManager::new(1));
        let task_id = mgr.register("calls");

        let state = super::wait_for_task_completion(&mgr, &task_id, Duration::from_millis(1)).await;

        assert!(state.is_none());
        assert!(matches!(
            mgr.poll(&task_id).unwrap().status,
            TaskStatus::Pending
        ));
    }
}
