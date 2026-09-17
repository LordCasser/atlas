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
const TOOL_CATALOG_TTL_MS: u64 = 300_000;
const EXPLORE_TOOL_NAME: &str = "explore";
const EXPLORE_CANDIDATE_INPUT_ID: &str = "explore_symbol_selection";
const EXPLORE_CANDIDATE_FIELD: &str = "selection";
const PROJECT_TOOL_NAME: &str = "project";
const PROJECT_PATH_INPUT_ID: &str = "project_path_input";
const PROJECT_PATH_FIELD: &str = "project_path";
const DOMAIN_RULES_TOOL_NAME: &str = "domain_rules";
const FP_DISPATCHES_TOOL_NAME: &str = "fp_dispatches";
const OVERLAY_DELETE_CONFIRMATION_INPUT_ID: &str = "confirm_overlay_delete";
const OVERLAY_DELETE_CONFIRMATION_FIELD: &str = "confirm";
const OVERLAY_DELETE_CONFIRMATION_TTL_SECS: u64 = 300;
const MAX_PENDING_OVERLAY_DELETE_CONFIRMATIONS: usize = 128;
const QUERY_TASK_TTL_MS: u64 = tools::query_snapshot::QUERY_SNAPSHOT_TTL_SECS * 1_000;
const MIN_QUERY_TASK_POLL_MS: u64 = 250;
const MAX_QUERY_TASK_POLL_MS: u64 = 60_000;

#[derive(Debug)]
struct PreparedToolCall {
    args: serde_json::Value,
    allow_candidate_mrtr: bool,
    immediate_response: Option<rmcp_model::CallToolResponse>,
}

struct PreparedAdapterToolCall {
    call: PreparedToolCall,
    execution_router: Option<Arc<ToolRouter>>,
}

impl std::fmt::Debug for PreparedAdapterToolCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedAdapterToolCall")
            .field("call", &self.call)
            .field("has_execution_router", &self.execution_router.is_some())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum OverlayDeleteTarget {
    DomainRule(String),
    FpAnnotation(String),
    FpField(String),
}

impl OverlayDeleteTarget {
    fn description(&self) -> String {
        match self {
            Self::DomainRule(rule_id) => format!("domain rule `{rule_id}`"),
            Self::FpAnnotation(annotation_id) => {
                format!("function-pointer annotation `{annotation_id}`")
            }
            Self::FpField(field_qname) => {
                format!("function-pointer annotation for field `{field_qname}`")
            }
        }
    }

    fn result_fields(&self) -> serde_json::Value {
        match self {
            Self::DomainRule(rule_id) => serde_json::json!({
                "target_kind": "domain_rule",
                "rule_id": rule_id,
            }),
            Self::FpAnnotation(annotation_id) => serde_json::json!({
                "target_kind": "fp_annotation",
                "annotation_id": annotation_id,
            }),
            Self::FpField(field_qname) => serde_json::json!({
                "target_kind": "fp_field",
                "field_qname": field_qname,
            }),
        }
    }
}

struct PendingOverlayDeleteConfirmation {
    tool_name: String,
    args: serde_json::Value,
    execution_router: Arc<ToolRouter>,
    project_root: std::path::PathBuf,
    target: OverlayDeleteTarget,
    created_at: std::time::Instant,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RetryableQuery {
    query_id: String,
    retry_after_ms: u64,
}

struct PreparedQueryTask {
    retry: RetryableQuery,
    replay: tools::PinnedQueryReplay,
    allow_candidate_input: bool,
}

struct QueryTaskRun {
    replay_router: Arc<ToolRouter>,
    blocking_gate: Arc<tokio::sync::Semaphore>,
    retry: RetryableQuery,
    tool_name: String,
    tool_args: serde_json::Value,
    allow_candidate_input: bool,
}

#[derive(Debug, PartialEq)]
enum ExploreCandidateDecision {
    Accept(serde_json::Value),
    Decline,
}

fn is_valid_gregorian_date(year: u16, month: u8, day: u8) -> bool {
    if year == 0 {
        return false;
    }
    let leap_year =
        year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap_year => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days_in_month).contains(&day)
}

fn protocol_supports_sep_2322(protocol_version: Option<&rmcp_model::ProtocolVersion>) -> bool {
    let Some(version) = protocol_version.map(rmcp_model::ProtocolVersion::as_str) else {
        return false;
    };
    let bytes = version.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
    {
        return false;
    }

    let Ok(year) = version[0..4].parse::<u16>() else {
        return false;
    };
    let Ok(month) = version[5..7].parse::<u8>() else {
        return false;
    };
    let Ok(day) = version[8..10].parse::<u8>() else {
        return false;
    };
    is_valid_gregorian_date(year, month, day) && (year, month, day) >= (2026, 7, 28)
}

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
    task_manager: rmcp::task_manager::TaskManager,
    overlay_delete_confirmations:
        std::sync::Mutex<std::collections::HashMap<String, PendingOverlayDeleteConfirmation>>,
}

impl AtlasMcpService {
    fn new(store: Arc<Store>, project_root: std::path::PathBuf) -> Self {
        Self {
            router: Arc::new(ToolRouter::new_empty(store, project_root)),
            blocking_gate: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TOOL_CALLS)),
            task_manager: rmcp::task_manager::TaskManager::new(),
            overlay_delete_confirmations: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn new_unopened() -> Self {
        Self {
            router: Arc::new(ToolRouter::new_unopened()),
            blocking_gate: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_TOOL_CALLS)),
            task_manager: rmcp::task_manager::TaskManager::new(),
            overlay_delete_confirmations: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn to_rmcp_tool(tool: protocol::Tool) -> rmcp_model::Tool {
        rmcp_model::Tool::new_with_raw(
            tool.name,
            Some(tool.description.into()),
            tool.input_schema.into_object(),
        )
    }

    fn to_rmcp_result(result: protocol::CallToolResult) -> rmcp_model::CallToolResult {
        let content = result
            .content
            .into_iter()
            .map(|block| match block {
                protocol::ContentBlock::Text { text } => rmcp_model::ContentBlock::text(text),
            })
            .collect();

        if result.is_error.unwrap_or(false) {
            rmcp_model::CallToolResult::error(content)
        } else {
            rmcp_model::CallToolResult::success(content)
        }
    }

    fn list_tools_result(
        tools: Vec<rmcp_model::Tool>,
        protocol_version: Option<&rmcp_model::ProtocolVersion>,
    ) -> rmcp_model::ListToolsResult {
        let result = rmcp_model::ListToolsResult::with_all_items(tools);
        if protocol_supports_sep_2322(protocol_version) {
            // Safe as public because this catalog is fixed in the binary and does not depend on
            // the project, user, or authorization. Re-evaluate this scope if it becomes dynamic.
            result
                .with_ttl_ms(TOOL_CATALOG_TTL_MS)
                .with_cache_scope(rmcp_model::CacheScope::Public)
        } else {
            result
        }
    }

    fn form_mrtr_supported(
        protocol_version: Option<&rmcp_model::ProtocolVersion>,
        client_capabilities: Option<&rmcp_model::ClientCapabilities>,
    ) -> bool {
        protocol_supports_sep_2322(protocol_version)
            && client_capabilities
                .and_then(|capabilities| capabilities.elicitation.as_ref())
                .and_then(|elicitation| elicitation.form.as_ref())
                .is_some()
    }

    fn overlay_delete_target(
        tool_name: &str,
        args: &serde_json::Value,
    ) -> Option<OverlayDeleteTarget> {
        if args.get("action").and_then(serde_json::Value::as_str) != Some("delete") {
            return None;
        }

        match tool_name {
            DOMAIN_RULES_TOOL_NAME => args
                .get("rule_id")
                .and_then(serde_json::Value::as_str)
                .filter(|rule_id| !rule_id.is_empty())
                .map(|rule_id| OverlayDeleteTarget::DomainRule(rule_id.to_string())),
            FP_DISPATCHES_TOOL_NAME => args
                .get("annotation_id")
                .and_then(serde_json::Value::as_str)
                .filter(|annotation_id| !annotation_id.is_empty())
                .map(|annotation_id| OverlayDeleteTarget::FpAnnotation(annotation_id.to_string()))
                .or_else(|| {
                    args.get("field_qname")
                        .and_then(serde_json::Value::as_str)
                        .filter(|field_qname| !field_qname.is_empty())
                        .map(|field_qname| OverlayDeleteTarget::FpField(field_qname.to_string()))
                }),
            _ => None,
        }
    }

    fn prune_overlay_delete_confirmations(
        confirmations: &mut std::collections::HashMap<String, PendingOverlayDeleteConfirmation>,
    ) {
        let ttl = std::time::Duration::from_secs(OVERLAY_DELETE_CONFIRMATION_TTL_SECS);
        confirmations.retain(|_, pending| pending.created_at.elapsed() < ttl);
    }

    fn overlay_delete_confirmation_response(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        target: OverlayDeleteTarget,
    ) -> Result<PreparedAdapterToolCall, rmcp::ErrorData> {
        let (execution_router, project_root) =
            self.router.pin_active_project_scope().ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    "overlay delete confirmation requires an active project",
                    None,
                )
            })?;
        let project_display = project_root.to_string_lossy().into_owned();
        let target_display = target.description();
        let requested_schema = rmcp_model::ElicitationSchema::builder()
            .required_bool_property(OVERLAY_DELETE_CONFIRMATION_FIELD, |schema| {
                schema
                    .title("Confirm persistent deletion")
                    .description(
                        "Set true only after verifying the project and exact delete target",
                    )
                    .with_default(false)
            })
            .description("Confirm deletion of persistent Atlas overlay data")
            .build()
            .map_err(|error| {
                rmcp::ErrorData::internal_error(
                    "Failed to build overlay delete confirmation schema",
                    Some(serde_json::json!({ "detail": error })),
                )
            })?;
        let request = rmcp_model::ElicitRequest::new(
            rmcp_model::ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: format!(
                    "Confirm deleting {target_display} from Atlas project `{project_display}`. This modifies persistent project data."
                ),
                requested_schema,
            },
        );
        let mut input_requests = std::collections::BTreeMap::new();
        input_requests.insert(
            OVERLAY_DELETE_CONFIRMATION_INPUT_ID.into(),
            rmcp_model::InputRequest::Elicitation(request),
        );

        let mut confirmations = self
            .overlay_delete_confirmations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::prune_overlay_delete_confirmations(&mut confirmations);
        if confirmations.len() >= MAX_PENDING_OVERLAY_DELETE_CONFIRMATIONS {
            return Err(rmcp::ErrorData::internal_error(
                "Atlas overlay delete confirmation queue is full",
                Some(serde_json::json!({
                    "maximum_pending": MAX_PENDING_OVERLAY_DELETE_CONFIRMATIONS,
                })),
            ));
        }
        let request_state = loop {
            let candidate = uuid::Uuid::new_v4().to_string();
            if !confirmations.contains_key(&candidate) {
                break candidate;
            }
        };
        confirmations.insert(
            request_state.clone(),
            PendingOverlayDeleteConfirmation {
                tool_name: tool_name.to_string(),
                args: args.clone(),
                execution_router: Arc::new(execution_router),
                project_root,
                target,
                created_at: std::time::Instant::now(),
            },
        );

        Ok(PreparedAdapterToolCall {
            call: PreparedToolCall {
                args: args.clone(),
                allow_candidate_mrtr: false,
                immediate_response: Some(rmcp_model::CallToolResponse::InputRequired(
                    rmcp_model::InputRequiredResult::new(Some(input_requests), Some(request_state)),
                )),
            },
            execution_router: None,
        })
    }

    fn take_overlay_delete_confirmation(
        &self,
        request_state: &str,
    ) -> Result<PendingOverlayDeleteConfirmation, rmcp::ErrorData> {
        let mut confirmations = self
            .overlay_delete_confirmations
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Self::prune_overlay_delete_confirmations(&mut confirmations);
        confirmations.remove(request_state).ok_or_else(|| {
            rmcp::ErrorData::invalid_params(
                "overlay delete confirmation state is unknown, expired, or already used",
                None,
            )
        })
    }

    fn overlay_delete_declined_response(
        pending: &PendingOverlayDeleteConfirmation,
    ) -> rmcp_model::CallToolResponse {
        let mut body = pending.target.result_fields();
        if let serde_json::Value::Object(fields) = &mut body {
            fields.insert("ok".into(), serde_json::Value::Bool(false));
            fields.insert(
                "status".into(),
                serde_json::Value::String("not_deleted".into()),
            );
            fields.insert(
                "reason".into(),
                serde_json::Value::String("confirmation_declined".into()),
            );
            fields.insert(
                "tool".into(),
                serde_json::Value::String(pending.tool_name.clone()),
            );
            fields.insert(
                "project_path".into(),
                serde_json::Value::String(pending.project_root.to_string_lossy().into_owned()),
            );
        }
        rmcp_model::CallToolResponse::Complete(Self::to_rmcp_result(protocol::CallToolResult {
            content: vec![protocol::ContentBlock::text(body.to_string())],
            is_error: Some(false),
        }))
    }

    fn prepare_overlay_delete_retry(
        &self,
        tool_name: &str,
        args: &serde_json::Value,
        input_responses: Option<&rmcp_model::InputResponses>,
        request_state: &str,
    ) -> Result<PreparedAdapterToolCall, rmcp::ErrorData> {
        // Removing first makes every signed offer one-shot, including malformed,
        // mismatched, and failed delete attempts.
        let pending = self.take_overlay_delete_confirmation(request_state)?;
        if pending.tool_name != tool_name || pending.args != *args {
            return Err(rmcp::ErrorData::invalid_params(
                "overlay delete confirmation retry changed the original tool or arguments",
                None,
            ));
        }
        let responses = input_responses.ok_or_else(|| {
            rmcp::ErrorData::invalid_params(
                "overlay delete confirmation inputResponses are missing",
                None,
            )
        })?;
        if responses.len() != 1 {
            return Err(rmcp::ErrorData::invalid_params(
                "overlay delete confirmation expects exactly one input response",
                None,
            ));
        }
        let response = responses
            .get(OVERLAY_DELETE_CONFIRMATION_INPUT_ID)
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    "overlay delete confirmation response is missing",
                    None,
                )
            })?;
        let action = response
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    "overlay delete confirmation action is missing",
                    None,
                )
            })?;

        match action {
            "accept" => {
                let confirmed = response
                    .get("content")
                    .and_then(|content| content.get(OVERLAY_DELETE_CONFIRMATION_FIELD))
                    .and_then(serde_json::Value::as_bool)
                    .ok_or_else(|| {
                        rmcp::ErrorData::invalid_params(
                            "accepted overlay delete confirmation requires a boolean confirm value",
                            None,
                        )
                    })?;
                if confirmed {
                    Ok(PreparedAdapterToolCall {
                        call: PreparedToolCall {
                            args: pending.args,
                            allow_candidate_mrtr: false,
                            immediate_response: None,
                        },
                        execution_router: Some(pending.execution_router),
                    })
                } else {
                    Ok(PreparedAdapterToolCall {
                        call: PreparedToolCall {
                            args: pending.args.clone(),
                            allow_candidate_mrtr: false,
                            immediate_response: Some(Self::overlay_delete_declined_response(
                                &pending,
                            )),
                        },
                        execution_router: None,
                    })
                }
            }
            "decline" | "cancel" => Ok(PreparedAdapterToolCall {
                call: PreparedToolCall {
                    args: pending.args.clone(),
                    allow_candidate_mrtr: false,
                    immediate_response: Some(Self::overlay_delete_declined_response(&pending)),
                },
                execution_router: None,
            }),
            _ => Err(rmcp::ErrorData::invalid_params(
                "unknown overlay delete confirmation action",
                None,
            )),
        }
    }

    fn prepare_adapter_tool_call(
        &self,
        tool_name: &str,
        args: serde_json::Value,
        input_responses: Option<&rmcp_model::InputResponses>,
        request_state: Option<&str>,
        form_mrtr_supported: bool,
    ) -> Result<PreparedAdapterToolCall, rmcp::ErrorData> {
        if let Some(request_state) = request_state {
            return self.prepare_overlay_delete_retry(
                tool_name,
                &args,
                input_responses,
                request_state,
            );
        }

        let target = Self::overlay_delete_target(tool_name, &args);
        if target.is_some() && input_responses.is_some() {
            return Err(rmcp::ErrorData::invalid_params(
                "overlay delete confirmation requestState is missing",
                None,
            ));
        }
        if input_responses.is_none()
            && form_mrtr_supported
            && let Some(target) = target
            && self.router.project.is_active()
        {
            return self.overlay_delete_confirmation_response(tool_name, &args, target);
        }

        if input_responses.is_some()
            && tool_name != PROJECT_TOOL_NAME
            && tool_name != EXPLORE_TOOL_NAME
        {
            return Err(rmcp::ErrorData::invalid_params(
                "this tool does not accept MCP inputResponses",
                None,
            ));
        }

        Ok(PreparedAdapterToolCall {
            call: Self::prepare_tool_call(
                tool_name,
                args,
                input_responses,
                None,
                form_mrtr_supported,
            )?,
            execution_router: None,
        })
    }

    fn query_tasks_supported(
        protocol_version: Option<&rmcp_model::ProtocolVersion>,
        client_capabilities: Option<&rmcp_model::ClientCapabilities>,
    ) -> bool {
        protocol_supports_sep_2322(protocol_version)
            && client_capabilities.is_some_and(rmcp_model::ClientCapabilities::supports_tasks)
    }

    fn query_task_supported_for_tool(
        tool_name: &str,
        query_tasks_supported: bool,
        form_mrtr_supported: bool,
    ) -> bool {
        query_tasks_supported && (tool_name != EXPLORE_TOOL_NAME || form_mrtr_supported)
    }

    fn retryable_query(result: &protocol::CallToolResult) -> Option<RetryableQuery> {
        if result.is_error.unwrap_or(false) || result.content.len() != 1 {
            return None;
        }
        let text = match result.content.first()? {
            protocol::ContentBlock::Text { text } => text,
        };
        let body: serde_json::Value = serde_json::from_str(text).ok()?;
        let query_id = body.get("query_id")?.as_str()?.trim();
        if query_id.is_empty() {
            return None;
        }
        let retry_after_ms = body
            .get("analysis")?
            .get("retry_after_ms")?
            .as_u64()
            .filter(|retry_after_ms| *retry_after_ms > 0)?;
        Some(RetryableQuery {
            query_id: query_id.to_string(),
            retry_after_ms,
        })
    }

    fn bounded_query_task_poll_ms(retry_after_ms: u64) -> u64 {
        retry_after_ms.clamp(MIN_QUERY_TASK_POLL_MS, MAX_QUERY_TASK_POLL_MS)
    }

    fn project_open_missing_path(args: &serde_json::Value) -> bool {
        args.get("action").and_then(serde_json::Value::as_str) == Some("open")
            && match args.get(PROJECT_PATH_FIELD) {
                None => true,
                Some(serde_json::Value::String(path)) => path.is_empty(),
                Some(_) => false,
            }
    }

    fn project_path_input_required() -> Result<rmcp_model::CallToolResponse, rmcp::ErrorData> {
        let requested_schema = rmcp_model::ElicitationSchema::builder()
            .required_string_with(PROJECT_PATH_FIELD, |schema| {
                schema
                    .title("Project directory")
                    .description("Absolute path to the project directory Atlas should open")
                    .length(1, tools::MAX_FILE_PATH_LENGTH as u32)
            })
            .description("Provide the project directory required by project(action=\"open\")")
            .build()
            .map_err(|error| {
                rmcp::ErrorData::internal_error(
                    "Failed to build project path elicitation schema",
                    Some(serde_json::json!({ "detail": error })),
                )
            })?;
        let request = rmcp_model::ElicitRequest::new(
            rmcp_model::ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "Choose the project directory Atlas should open".into(),
                requested_schema,
            },
        );
        let mut input_requests = std::collections::BTreeMap::new();
        input_requests.insert(
            PROJECT_PATH_INPUT_ID.into(),
            rmcp_model::InputRequest::Elicitation(request),
        );
        Ok(rmcp_model::CallToolResponse::InputRequired(
            rmcp_model::InputRequiredResult::from_input_requests(input_requests),
        ))
    }

    fn prepare_project_path_call(
        args: serde_json::Value,
        input_responses: Option<&rmcp_model::InputResponses>,
        request_state: Option<&str>,
        form_mrtr_supported: bool,
    ) -> Result<PreparedToolCall, rmcp::ErrorData> {
        if input_responses.is_none() {
            if !Self::project_open_missing_path(&args) {
                return Ok(PreparedToolCall {
                    args,
                    allow_candidate_mrtr: false,
                    immediate_response: None,
                });
            }
            if request_state.is_some() {
                return Err(rmcp::ErrorData::invalid_params(
                    "project path elicitation does not use requestState",
                    None,
                ));
            }
            if !form_mrtr_supported {
                return Ok(PreparedToolCall {
                    args,
                    allow_candidate_mrtr: false,
                    immediate_response: None,
                });
            }
            return Ok(PreparedToolCall {
                args,
                allow_candidate_mrtr: false,
                immediate_response: Some(Self::project_path_input_required()?),
            });
        }

        if !form_mrtr_supported {
            return Err(rmcp::ErrorData::invalid_params(
                "project path elicitation requires MCP 2026-07-28 and form elicitation",
                None,
            ));
        }
        if request_state.is_some() {
            return Err(rmcp::ErrorData::invalid_params(
                "project path elicitation does not use requestState",
                None,
            ));
        }

        let responses = input_responses.ok_or_else(|| {
            rmcp::ErrorData::invalid_params("project path inputResponses are missing", None)
        })?;
        if responses.len() != 1 {
            return Err(rmcp::ErrorData::invalid_params(
                "project path elicitation expects exactly one input response",
                None,
            ));
        }
        let response = responses.get(PROJECT_PATH_INPUT_ID).ok_or_else(|| {
            rmcp::ErrorData::invalid_params("project path elicitation response is missing", None)
        })?;
        let action = response
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params("project path elicitation action is missing", None)
            })?;
        let mut arguments = match args {
            serde_json::Value::Object(arguments) => arguments,
            _ => {
                return Err(rmcp::ErrorData::invalid_params(
                    "project path elicitation retry must preserve the original arguments",
                    None,
                ));
            }
        };
        if arguments.get("action").and_then(serde_json::Value::as_str) != Some("open") {
            return Err(rmcp::ErrorData::invalid_params(
                "project path elicitation retry must preserve action=open",
                None,
            ));
        }

        match action {
            "accept" => {
                let project_path = response
                    .get("content")
                    .and_then(|content| content.get(PROJECT_PATH_FIELD))
                    .and_then(serde_json::Value::as_str)
                    .filter(|path| !path.is_empty())
                    .ok_or_else(|| {
                        rmcp::ErrorData::invalid_params(
                            "accepted project path elicitation requires a non-empty string",
                            None,
                        )
                    })?;
                arguments.insert(
                    PROJECT_PATH_FIELD.into(),
                    serde_json::Value::String(project_path.to_string()),
                );
            }
            "decline" | "cancel" => {
                arguments.remove(PROJECT_PATH_FIELD);
            }
            _ => {
                return Err(rmcp::ErrorData::invalid_params(
                    "unknown project path elicitation action",
                    None,
                ));
            }
        }

        Ok(PreparedToolCall {
            args: serde_json::Value::Object(arguments),
            allow_candidate_mrtr: false,
            immediate_response: None,
        })
    }

    fn prepare_tool_call(
        tool_name: &str,
        args: serde_json::Value,
        input_responses: Option<&rmcp_model::InputResponses>,
        request_state: Option<&str>,
        form_mrtr_supported: bool,
    ) -> Result<PreparedToolCall, rmcp::ErrorData> {
        if tool_name == PROJECT_TOOL_NAME {
            return Self::prepare_project_path_call(
                args,
                input_responses,
                request_state,
                form_mrtr_supported,
            );
        }
        if tool_name != EXPLORE_TOOL_NAME {
            return Ok(PreparedToolCall {
                args,
                allow_candidate_mrtr: false,
                immediate_response: None,
            });
        }

        if input_responses.is_none() && request_state.is_none() {
            return Ok(PreparedToolCall {
                args,
                allow_candidate_mrtr: form_mrtr_supported,
                immediate_response: None,
            });
        }

        if !form_mrtr_supported {
            return Err(rmcp::ErrorData::invalid_params(
                "explore candidate selection requires MCP 2026-07-28 and form elicitation",
                None,
            ));
        }
        if request_state.is_some() {
            return Err(rmcp::ErrorData::invalid_params(
                "explore candidate selection does not use requestState",
                None,
            ));
        }

        let responses = input_responses.ok_or_else(|| {
            rmcp::ErrorData::invalid_params(
                "explore candidate selection is missing inputResponses",
                None,
            )
        })?;
        if responses.len() != 1 {
            return Err(rmcp::ErrorData::invalid_params(
                "explore candidate selection expects exactly one input response",
                None,
            ));
        }
        let response = responses.get(EXPLORE_CANDIDATE_INPUT_ID).ok_or_else(|| {
            rmcp::ErrorData::invalid_params("explore candidate selection response is missing", None)
        })?;
        let decision = Self::prepare_explore_candidate_response(&args, response)?;
        let args = match decision {
            ExploreCandidateDecision::Accept(args) => args,
            ExploreCandidateDecision::Decline => args,
        };
        Ok(PreparedToolCall {
            args,
            allow_candidate_mrtr: false,
            immediate_response: None,
        })
    }

    fn prepare_explore_candidate_response(
        args: &serde_json::Value,
        response: &serde_json::Value,
    ) -> Result<ExploreCandidateDecision, rmcp::ErrorData> {
        let action = response
            .get("action")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                rmcp::ErrorData::invalid_params(
                    "explore candidate selection action is missing",
                    None,
                )
            })?;

        match action {
            "accept" => {
                let selection = response
                    .get("content")
                    .and_then(|content| content.get(EXPLORE_CANDIDATE_FIELD))
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        rmcp::ErrorData::invalid_params(
                            "accepted explore candidate selection is missing its value",
                            None,
                        )
                    })?;
                let selected: serde_json::Value =
                    serde_json::from_str(selection).map_err(|_| {
                        rmcp::ErrorData::invalid_params(
                            "explore candidate selection must be a JSON object",
                            None,
                        )
                    })?;
                if !selected.is_object() {
                    return Err(rmcp::ErrorData::invalid_params(
                        "explore candidate selection must be a JSON object",
                        None,
                    ));
                }
                let mut arguments = match args {
                    serde_json::Value::Object(arguments) => arguments.clone(),
                    _ => {
                        return Err(rmcp::ErrorData::invalid_params(
                            "explore candidate selection retry must preserve the original arguments",
                            None,
                        ));
                    }
                };
                arguments.insert("symbol".into(), selected);
                Ok(ExploreCandidateDecision::Accept(serde_json::Value::Object(
                    arguments,
                )))
            }
            "decline" | "cancel" => Ok(ExploreCandidateDecision::Decline),
            _ => Err(rmcp::ErrorData::invalid_params(
                "unknown explore candidate selection action",
                None,
            )),
        }
    }

    fn explore_candidate_input_request(
        result: &protocol::CallToolResult,
    ) -> Option<rmcp_model::InputRequest> {
        if result.is_error.unwrap_or(false) || result.content.len() != 1 {
            return None;
        }
        let text = match result.content.first()? {
            protocol::ContentBlock::Text { text } => text,
        };
        let body: serde_json::Value = serde_json::from_str(text).ok()?;
        if body.get("ambiguous").and_then(serde_json::Value::as_bool) != Some(true) {
            return None;
        }
        let candidates = body
            .get("candidates")
            .and_then(serde_json::Value::as_array)?;
        if candidates.len() < 2 {
            return None;
        }

        let mut values = Vec::with_capacity(candidates.len());
        let mut titles = Vec::with_capacity(candidates.len());
        for candidate in candidates {
            let symbol_ref = candidate.get("symbol_ref")?;
            if !symbol_ref.is_object() {
                return None;
            }
            values.push(serde_json::to_string(symbol_ref).ok()?);

            let qualified_name = candidate
                .get("qualified_name")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    symbol_ref
                        .get("qualified_name")
                        .and_then(serde_json::Value::as_str)
                })?;
            let file_path = candidate
                .get("file_path")
                .and_then(serde_json::Value::as_str)?;
            let line = candidate.get("line").and_then(serde_json::Value::as_u64)?;
            titles.push(format!("{qualified_name} — {file_path}:{line}"));
        }

        let enum_schema = rmcp_model::EnumSchema::builder(values)
            .enum_titles(titles)
            .ok()?
            .build();
        let requested_schema = rmcp_model::ElicitationSchema::builder()
            .required_enum_schema(EXPLORE_CANDIDATE_FIELD, enum_schema)
            .description("Choose the exact symbol definition to explore")
            .build()
            .ok()?;
        let request = rmcp_model::ElicitRequest::new(
            rmcp_model::ElicitRequestParams::FormElicitationParams {
                meta: None,
                message: "Choose the symbol definition Atlas should explore".into(),
                requested_schema,
            },
        );
        Some(rmcp_model::InputRequest::Elicitation(request))
    }

    fn explore_candidate_input_required(
        result: &protocol::CallToolResult,
    ) -> Option<rmcp_model::InputRequiredResult> {
        let mut input_requests = std::collections::BTreeMap::new();
        input_requests.insert(
            EXPLORE_CANDIDATE_INPUT_ID.into(),
            Self::explore_candidate_input_request(result)?,
        );
        Some(rmcp_model::InputRequiredResult::from_input_requests(
            input_requests,
        ))
    }

    fn to_rmcp_response(
        tool_name: &str,
        result: protocol::CallToolResult,
        allow_candidate_mrtr: bool,
    ) -> rmcp_model::CallToolResponse {
        if tool_name == EXPLORE_TOOL_NAME
            && allow_candidate_mrtr
            && let Some(input_required) = Self::explore_candidate_input_required(&result)
        {
            rmcp_model::CallToolResponse::InputRequired(input_required)
        } else {
            rmcp_model::CallToolResponse::Complete(Self::to_rmcp_result(result))
        }
    }

    #[cfg(test)]
    fn maybe_spawn_query_task(
        task_manager: &rmcp::task_manager::TaskManager,
        router: &ToolRouter,
        blocking_gate: Arc<tokio::sync::Semaphore>,
        tool_name: &str,
        query_tasks_supported: bool,
        form_mrtr_supported: bool,
        result: &protocol::CallToolResult,
    ) -> Option<rmcp_model::CallToolResponse> {
        if !Self::query_task_supported_for_tool(
            tool_name,
            query_tasks_supported,
            form_mrtr_supported,
        ) {
            return None;
        }
        Self::spawn_query_task(
            task_manager,
            router,
            blocking_gate,
            tool_name,
            form_mrtr_supported,
            result,
        )
    }

    #[cfg(test)]
    fn spawn_query_task(
        task_manager: &rmcp::task_manager::TaskManager,
        router: &ToolRouter,
        blocking_gate: Arc<tokio::sync::Semaphore>,
        tool_name: &str,
        allow_candidate_input: bool,
        result: &protocol::CallToolResult,
    ) -> Option<rmcp_model::CallToolResponse> {
        let prepared = Self::prepare_query_task(router, tool_name, allow_candidate_input, result)?;
        Some(Self::spawn_prepared_query_task(
            task_manager,
            blocking_gate,
            prepared,
        ))
    }

    fn prepare_query_task(
        router: &ToolRouter,
        tool_name: &str,
        allow_candidate_input: bool,
        result: &protocol::CallToolResult,
    ) -> Option<PreparedQueryTask> {
        let retry = Self::retryable_query(result)?;
        let replay = router.pin_query_replay(&retry.query_id)?;
        if replay.tool_name != tool_name {
            return None;
        }
        Some(PreparedQueryTask {
            retry,
            replay,
            allow_candidate_input: allow_candidate_input && tool_name == EXPLORE_TOOL_NAME,
        })
    }

    fn spawn_prepared_query_task(
        task_manager: &rmcp::task_manager::TaskManager,
        blocking_gate: Arc<tokio::sync::Semaphore>,
        prepared: PreparedQueryTask,
    ) -> rmcp_model::CallToolResponse {
        let PreparedQueryTask {
            retry,
            replay,
            allow_candidate_input,
        } = prepared;
        let poll_interval_ms = Self::bounded_query_task_poll_ms(retry.retry_after_ms);
        let run = QueryTaskRun {
            replay_router: Arc::new(replay.router),
            blocking_gate,
            retry,
            tool_name: replay.tool_name,
            tool_args: replay.tool_args,
            allow_candidate_input,
        };
        let task = task_manager.spawn(
            rmcp::task_manager::TaskOptions::new()
                .with_ttl_ms(QUERY_TASK_TTL_MS)
                .with_poll_interval_ms(poll_interval_ms)
                .with_status_message("Atlas is completing Focus analysis"),
            move |task_context| Box::pin(Self::run_query_task(task_context, run)),
        );
        rmcp_model::CallToolResponse::Task(rmcp_model::CreateTaskResult::new(task))
    }

    async fn run_query_task(
        task_context: rmcp::task_manager::TaskContext,
        run: QueryTaskRun,
    ) -> Result<rmcp_model::CallToolResult, rmcp::task_manager::TaskExit> {
        Self::run_query_task_with_ttl(
            task_context,
            run,
            std::time::Duration::from_millis(QUERY_TASK_TTL_MS),
        )
        .await
    }

    async fn run_query_task_with_ttl(
        task_context: rmcp::task_manager::TaskContext,
        run: QueryTaskRun,
        ttl: std::time::Duration,
    ) -> Result<rmcp_model::CallToolResult, rmcp::task_manager::TaskExit> {
        let QueryTaskRun {
            replay_router,
            blocking_gate,
            mut retry,
            tool_name,
            tool_args,
            allow_candidate_input,
        } = run;
        let deadline = tokio::time::Instant::now() + ttl;
        let mut candidate_input_used = false;
        loop {
            let delay = std::time::Duration::from_millis(Self::bounded_query_task_poll_ms(
                retry.retry_after_ms,
            ));
            tokio::select! {
                _ = task_context.cancelled() => {
                    return Err(rmcp::task_manager::TaskExit::Cancelled);
                }
                _ = tokio::time::sleep_until(deadline) => {
                    return Err(Self::query_task_expired());
                }
                _ = tokio::time::sleep(delay) => {}
            }

            let result = Self::run_query_task_tool_call(
                &task_context,
                Arc::clone(&replay_router),
                Arc::clone(&blocking_gate),
                deadline,
                "resume_query",
                serde_json::json!({"query_id": retry.query_id}),
            )
            .await?;

            if let Some(next_retry) = Self::retryable_query(&result) {
                if next_retry.query_id != retry.query_id {
                    return Err(rmcp::task_manager::TaskExit::Error(
                        rmcp::ErrorData::internal_error(
                            "Atlas task replay changed query identity",
                            Some(serde_json::json!({
                                "expected": retry.query_id,
                                "actual": next_retry.query_id,
                            })),
                        ),
                    ));
                }
                retry = next_retry;
                continue;
            }

            if allow_candidate_input
                && !candidate_input_used
                && tool_name == EXPLORE_TOOL_NAME
                && let Some(input_request) = Self::explore_candidate_input_request(&result)
            {
                candidate_input_used = true;
                task_context.set_status_message("Atlas needs an exact symbol selection");
                let response = tokio::select! {
                    _ = task_context.cancelled() => {
                        return Err(rmcp::task_manager::TaskExit::Cancelled);
                    }
                    _ = tokio::time::sleep_until(deadline) => {
                        return Err(Self::query_task_expired());
                    }
                    response = task_context.request_input(
                        EXPLORE_CANDIDATE_INPUT_ID,
                        input_request,
                    ) => response?,
                };

                match Self::prepare_explore_candidate_response(&tool_args, &response)
                    .map_err(rmcp::task_manager::TaskExit::Error)?
                {
                    ExploreCandidateDecision::Decline => {
                        return Ok(Self::to_rmcp_result(result));
                    }
                    ExploreCandidateDecision::Accept(selected_args) => {
                        task_context
                            .set_status_message("Atlas is completing the selected Focus analysis");
                        let selected_result = Self::run_query_task_tool_call(
                            &task_context,
                            Arc::clone(&replay_router),
                            Arc::clone(&blocking_gate),
                            deadline,
                            EXPLORE_TOOL_NAME,
                            selected_args,
                        )
                        .await?;
                        if let Some(next_retry) = Self::retryable_query(&selected_result) {
                            let Some(next_replay) = replay_router
                                .pin_query_replay(&next_retry.query_id)
                                .filter(|replay| replay.tool_name == EXPLORE_TOOL_NAME)
                            else {
                                return Err(rmcp::task_manager::TaskExit::Error(
                                    rmcp::ErrorData::internal_error(
                                        "Atlas selected explore retry could not bind its snapshot",
                                        Some(serde_json::json!({
                                            "query_id": next_retry.query_id,
                                        })),
                                    ),
                                ));
                            };
                            retry = next_retry;
                            drop(next_replay);
                            continue;
                        }
                        return Ok(Self::to_rmcp_result(selected_result));
                    }
                }
            }

            return Ok(Self::to_rmcp_result(result));
        }
    }

    async fn run_query_task_tool_call(
        task_context: &rmcp::task_manager::TaskContext,
        router: Arc<ToolRouter>,
        blocking_gate: Arc<tokio::sync::Semaphore>,
        deadline: tokio::time::Instant,
        tool_name: &'static str,
        args: serde_json::Value,
    ) -> Result<protocol::CallToolResult, rmcp::task_manager::TaskExit> {
        let permit = tokio::select! {
            _ = task_context.cancelled() => {
                return Err(rmcp::task_manager::TaskExit::Cancelled);
            }
            _ = tokio::time::sleep_until(deadline) => {
                return Err(Self::query_task_expired());
            }
            permit = blocking_gate.acquire_owned() => {
                permit.map_err(|error| {
                    rmcp::task_manager::TaskExit::Error(rmcp::ErrorData::internal_error(
                        "Atlas tool gate closed while executing task",
                        Some(serde_json::json!({ "detail": error.to_string() })),
                    ))
                })?
            }
        };

        let worker = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            router.call_tool(&tools::ToolCallContext::empty(), tool_name, &args)
        });
        Self::await_query_task_worker(task_context, deadline, worker).await
    }

    /// Settle the visible task at cancellation/deadline even if a synchronous
    /// call has already started. `abort()` prevents queued blocking work but
    /// cannot forcibly stop a running `spawn_blocking`; such work remains
    /// detached and keeps its permit until the core call returns.
    async fn await_query_task_worker(
        task_context: &rmcp::task_manager::TaskContext,
        deadline: tokio::time::Instant,
        mut worker: tokio::task::JoinHandle<protocol::CallToolResult>,
    ) -> Result<protocol::CallToolResult, rmcp::task_manager::TaskExit> {
        let result = tokio::select! {
            _ = task_context.cancelled() => {
                worker.abort();
                return Err(rmcp::task_manager::TaskExit::Cancelled);
            }
            _ = tokio::time::sleep_until(deadline) => {
                worker.abort();
                return Err(Self::query_task_expired());
            }
            result = &mut worker => result,
        }
        .map_err(|error| {
            rmcp::task_manager::TaskExit::Error(rmcp::ErrorData::internal_error(
                "Atlas task tool worker failed",
                Some(serde_json::json!({ "detail": error.to_string() })),
            ))
        })?;

        if task_context.is_cancel_requested() {
            return Err(rmcp::task_manager::TaskExit::Cancelled);
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(Self::query_task_expired());
        }
        Ok(result)
    }

    fn query_task_expired() -> rmcp::task_manager::TaskExit {
        rmcp::task_manager::TaskExit::Error(rmcp::ErrorData::internal_error(
            "Atlas query task expired before Focus analysis completed",
            None,
        ))
    }
}

impl Drop for AtlasMcpService {
    fn drop(&mut self) {
        self.task_manager.shutdown();
    }
}

impl ServerHandler for AtlasMcpService {
    fn get_info(&self) -> rmcp_model::ServerInfo {
        rmcp_model::ServerInfo::new(
            rmcp_model::ServerCapabilities::builder()
                .enable_tools()
                .enable_tasks()
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
        context: RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<rmcp_model::ListToolsResult, rmcp::ErrorData>> + Send + '_
    {
        let protocol_version = context.protocol_version();
        let tools = self
            .router
            .list_tools()
            .tools
            .into_iter()
            .map(Self::to_rmcp_tool)
            .collect();
        let result = Ok(Self::list_tools_result(tools, protocol_version.as_ref()));
        std::future::ready(result)
    }

    fn call_tool(
        &self,
        request: rmcp_model::CallToolRequestParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<rmcp_model::CallToolResponse, rmcp::ErrorData>> + Send + '_
    {
        let start = std::time::Instant::now();
        let tool_name = request.name.to_string();
        let progress_token = request.progress_token();
        let has_progress_token = progress_token.is_some();
        let protocol_version = context.protocol_version();
        let client_capabilities = context.client_capabilities();
        let form_mrtr_supported =
            Self::form_mrtr_supported(protocol_version.as_ref(), client_capabilities.as_ref());
        let query_tasks_supported =
            Self::query_tasks_supported(protocol_version.as_ref(), client_capabilities.as_ref());
        let input_responses = request.input_responses;
        let request_state = request.request_state;
        let args = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or(serde_json::Value::Null);
        let prepared = self.prepare_adapter_tool_call(
            &tool_name,
            args,
            input_responses.as_ref(),
            request_state.as_deref(),
            form_mrtr_supported,
        );
        let default_router = Arc::clone(&self.router);
        let blocking_gate = Arc::clone(&self.blocking_gate);
        let task_manager = self.task_manager.clone();

        async move {
            let PreparedAdapterToolCall {
                call:
                    PreparedToolCall {
                        args,
                        allow_candidate_mrtr,
                        immediate_response,
                    },
                execution_router,
            } = prepared?;
            let router = execution_router.unwrap_or(default_router);
            if let Some(response) = immediate_response {
                let duration_ms = start.elapsed().as_millis() as u64;
                let span = tracing::info_span!(
                    "mcp_request",
                    method = "tools/call",
                    tool_name = %tool_name,
                    tool_error = false,
                    duration_ms = duration_ms,
                    ok = true,
                );
                tracing::info!(parent: &span, "request requires additional input");
                return Ok(response);
            }
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
            let prepare_query_task = Self::query_task_supported_for_tool(
                &tool_name,
                query_tasks_supported,
                form_mrtr_supported,
            );
            let allow_candidate_input = tool_name == EXPLORE_TOOL_NAME && form_mrtr_supported;
            let execution_router = if prepare_query_task && tool_name != PROJECT_TOOL_NAME {
                Arc::new(router.pin_project_scope())
            } else {
                Arc::clone(&router)
            };
            let blocking_permit =
                Arc::clone(&blocking_gate)
                    .acquire_owned()
                    .await
                    .map_err(|error| {
                        rmcp::ErrorData::internal_error(
                            "Atlas tool gate closed",
                            Some(serde_json::json!({ "detail": error.to_string() })),
                        )
                    })?;
            let (tool_result, prepared_query_task) = tokio::task::spawn_blocking(move || {
                let _blocking_permit = blocking_permit;
                let result = execution_router.call_tool(&blocking_ctx, &blocking_tool_name, &args);
                let prepared = prepare_query_task
                    .then(|| {
                        Self::prepare_query_task(
                            execution_router.as_ref(),
                            &blocking_tool_name,
                            allow_candidate_input,
                            &result,
                        )
                    })
                    .flatten();
                (result, prepared)
            })
            .await
            .map_err(|error| {
                rmcp::ErrorData::internal_error(
                    "Atlas tool worker failed",
                    Some(serde_json::json!({ "detail": error.to_string() })),
                )
            })?;
            let tool_error = tool_result.is_error.unwrap_or(false);
            let task_response = prepared_query_task.map(|prepared| {
                Self::spawn_prepared_query_task(&task_manager, Arc::clone(&blocking_gate), prepared)
            });
            let task_created = task_response.is_some();
            let response = task_response.unwrap_or_else(|| {
                Self::to_rmcp_response(&tool_name, tool_result, allow_candidate_mrtr)
            });
            let duration_ms = start.elapsed().as_millis() as u64;
            let _span = tracing::info_span!(
                "mcp_request",
                method = "tools/call",
                tool_name = %tool_name,
                tool_error = tool_error,
                task_created = task_created,
                duration_ms = duration_ms,
                ok = !tool_error,
            );
            tracing::info!(parent: &_span, "request handled");
            let result = Ok(response);

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

    fn get_task(
        &self,
        request: rmcp_model::GetTaskParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<rmcp_model::GetTaskResult, rmcp::ErrorData>> + Send + '_ {
        let result = if protocol_supports_sep_2322(context.protocol_version().as_ref()) {
            self.task_manager
                .get_task(&request.task_id)
                .map(rmcp_model::GetTaskResult::new)
        } else {
            Err(rmcp::ErrorData::method_not_found::<rmcp_model::GetTaskMethod>())
        };
        std::future::ready(result)
    }

    fn update_task(
        &self,
        request: rmcp_model::UpdateTaskParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<(), rmcp::ErrorData>> + Send + '_ {
        let result = if protocol_supports_sep_2322(context.protocol_version().as_ref()) {
            self.task_manager
                .update_task(&request.task_id, request.input_responses)
        } else {
            Err(rmcp::ErrorData::method_not_found::<
                rmcp_model::UpdateTaskMethod,
            >())
        };
        std::future::ready(result)
    }

    fn cancel_task(
        &self,
        request: rmcp_model::CancelTaskParams,
        context: RequestContext<rmcp::RoleServer>,
    ) -> impl Future<Output = Result<(), rmcp::ErrorData>> + Send + '_ {
        let result = if protocol_supports_sep_2322(context.protocol_version().as_ref()) {
            self.task_manager.cancel_task(&request.task_id)
        } else {
            Err(rmcp::ErrorData::method_not_found::<
                rmcp_model::CancelTaskMethod,
            >())
        };
        std::future::ready(result)
    }
}

#[cfg(test)]
mod tests {
    use rmcp::ServiceExt as _;

    #[derive(Debug, Clone)]
    struct VersionedTasksClient {
        protocol_version: rmcp::model::ProtocolVersion,
    }

    impl rmcp::ClientHandler for VersionedTasksClient {
        fn get_info(&self) -> rmcp::model::ClientInfo {
            let mut info = rmcp::model::ClientInfo::default();
            info.protocol_version = self.protocol_version.clone();
            info.capabilities = rmcp::model::ClientCapabilities::builder()
                .enable_tasks()
                .build();
            info
        }
    }

    #[derive(Debug, Clone)]
    struct TaskInputClient;

    impl rmcp::ClientHandler for TaskInputClient {
        fn get_info(&self) -> rmcp::model::ClientInfo {
            let mut info = rmcp::model::ClientInfo::default();
            info.protocol_version = rmcp::model::ProtocolVersion::V_2026_07_28;
            info.capabilities = rmcp::model::ClientCapabilities::builder()
                .enable_tasks()
                .enable_elicitation_with(
                    rmcp::model::ElicitationCapability::new()
                        .with_form(rmcp::model::FormElicitationCapability::new()),
                )
                .build();
            info
        }
    }

    fn retryable_query_result(
        query_id: &str,
        retry_after_ms: u64,
    ) -> super::protocol::CallToolResult {
        super::protocol::CallToolResult {
            content: vec![super::protocol::ContentBlock::text(
                serde_json::json!({
                    "status": "in_progress",
                    "query_id": query_id,
                    "analysis": {"retry_after_ms": retry_after_ms}
                })
                .to_string(),
            )],
            is_error: Some(false),
        }
    }

    fn task_test_router(root: &std::path::Path) -> super::tools::ToolRouter {
        let store = atlas_engine::Store::open_in_memory().expect("in-memory task test store");
        store.init_schema().expect("task test schema");
        super::tools::ToolRouter::new_empty(std::sync::Arc::new(store), root.to_path_buf())
    }

    fn overlay_test_service(
        root: &std::path::Path,
    ) -> (super::AtlasMcpService, std::sync::Arc<atlas_engine::Store>) {
        let store = std::sync::Arc::new(
            atlas_engine::Store::open_in_memory().expect("in-memory overlay test store"),
        );
        store.init_schema().expect("overlay test schema");
        (
            super::AtlasMcpService::new(std::sync::Arc::clone(&store), root.to_path_buf()),
            store,
        )
    }

    fn overlay_confirmation_responses(
        action: &str,
        confirmed: Option<bool>,
    ) -> rmcp::model::InputResponses {
        let mut response = serde_json::json!({"action": action});
        if let Some(confirmed) = confirmed {
            response["content"] = serde_json::json!({
                super::OVERLAY_DELETE_CONFIRMATION_FIELD: confirmed,
            });
        }
        [(
            super::OVERLAY_DELETE_CONFIRMATION_INPUT_ID.to_string(),
            response,
        )]
        .into_iter()
        .collect()
    }

    fn issue_overlay_delete_confirmation(
        service: &super::AtlasMcpService,
        tool_name: &str,
        args: serde_json::Value,
    ) -> (String, rmcp::model::InputRequiredResult) {
        let prepared = service
            .prepare_adapter_tool_call(tool_name, args, None, None, true)
            .expect("modern overlay delete should request confirmation");
        assert!(prepared.execution_router.is_none());
        let Some(rmcp::model::CallToolResponse::InputRequired(result)) =
            prepared.call.immediate_response
        else {
            panic!("overlay delete should return input_required");
        };
        let request_state = result
            .request_state
            .clone()
            .expect("overlay delete confirmation has opaque requestState");
        (request_state, result)
    }

    fn call_tool_json(
        router: &super::tools::ToolRouter,
        tool_name: &str,
        args: &serde_json::Value,
    ) -> (serde_json::Value, bool) {
        let result = router.call_tool(&super::tools::ToolCallContext::empty(), tool_name, args);
        let text = match result.content.first().expect("tool returns text") {
            super::protocol::ContentBlock::Text { text } => text,
        };
        (
            serde_json::from_str(text).expect("tool result is JSON"),
            result.is_error.unwrap_or(false),
        )
    }

    fn complete_response_body(response: rmcp::model::CallToolResponse) -> serde_json::Value {
        let rmcp::model::CallToolResponse::Complete(result) = response else {
            panic!("expected complete tool response");
        };
        let wire = serde_json::to_value(result).expect("complete response serializes");
        let text = wire["content"][0]["text"]
            .as_str()
            .expect("complete response has text content");
        serde_json::from_str(text).expect("complete response text is JSON")
    }

    fn store_search_snapshot(router: &super::tools::ToolRouter, query_id: &str) {
        router.store_snapshot(super::tools::query_snapshot::QuerySnapshot {
            query_id: query_id.into(),
            tool_name: "search".into(),
            tool_args: serde_json::json!({"query": "task_probe_no_match"}),
            focus_result: None,
            created_at: std::time::Instant::now(),
            status: super::tools::query_snapshot::QueryStatus::Retryable,
        });
    }

    fn store_explore_snapshot(router: &super::tools::ToolRouter, query_id: &str) {
        router.store_snapshot(super::tools::query_snapshot::QuerySnapshot {
            query_id: query_id.into(),
            tool_name: super::EXPLORE_TOOL_NAME.into(),
            tool_args: serde_json::json!({
                "symbol": "shared_func",
                "source_mode": "none",
                "relation_limit": 7
            }),
            focus_result: None,
            created_at: std::time::Instant::now(),
            status: super::tools::query_snapshot::QueryStatus::Retryable,
        });
    }

    fn insert_task_symbol(router: &super::tools::ToolRouter, path: &str) {
        let store = router.store();
        let file_id = atlas_engine::FileId::generate(path);
        store
            .upsert_file(&atlas_engine::FileInfo {
                file_id,
                path: path.into(),
                language: atlas_engine::Language::TypeScript,
                content_hash: format!("hash-{path}"),
                status: atlas_engine::ParseStatus::Success,
            })
            .expect("task candidate test file");
        let symbol_id = atlas_engine::SymbolId::generate(
            &file_id,
            "typescript",
            "shared_func",
            "function",
            None,
        );
        store
            .insert_symbols(&[atlas_engine::SymbolDef {
                id: symbol_id,
                kind: atlas_engine::SymbolKind::Function,
                name: "shared_func".into(),
                qualified_name: "shared_func".into(),
                symbol_path: vec!["shared_func".into()],
                file_id,
                language: atlas_engine::Language::TypeScript,
                range: atlas_engine::TextRange::default(),
                name_range: atlas_engine::TextRange::default(),
                signature: None,
                visibility: None,
                exported: false,
                static_: false,
                async_: false,
                container: None,
                scope_id: None,
                package_name: None,
                namespace_path: vec![],
                layer: "structural".into(),
            }])
            .expect("task candidate test symbol");
    }

    async fn wait_for_task_status(
        manager: &rmcp::task_manager::TaskManager,
        task_id: &str,
        expected: rmcp::model::TaskStatus,
    ) -> rmcp::model::DetailedTask {
        for _ in 0..200 {
            let detailed = manager
                .get_task(task_id)
                .expect("task remains observable while waiting for status");
            if detailed.status() == expected {
                return detailed;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("task {task_id} did not reach {expected:?}");
    }

    fn task_candidate_selection(detailed: &rmcp::model::DetailedTask, index: usize) -> String {
        let rmcp::model::TaskPayload::InputRequired { input_requests } = &detailed.payload else {
            panic!("expected input_required task, got {:?}", detailed.payload);
        };
        let request = input_requests
            .get(super::EXPLORE_CANDIDATE_INPUT_ID)
            .expect("candidate input request is present");
        let wire = serde_json::to_value(request).expect("candidate input request serializes");
        wire["params"]["requestedSchema"]["properties"][super::EXPLORE_CANDIDATE_FIELD]["oneOf"]
            [index]["const"]
            .as_str()
            .expect("candidate selection is a JSON string")
            .to_string()
    }

    fn completed_task_body(detailed: rmcp::model::DetailedTask) -> serde_json::Value {
        let rmcp::model::TaskPayload::Completed { result } = detailed.payload else {
            panic!("expected completed task payload");
        };
        let text = result["content"][0]["text"]
            .as_str()
            .expect("completed task carries text content");
        serde_json::from_str(text).expect("completed task text is JSON")
    }

    fn ambiguous_explore_result() -> super::protocol::CallToolResult {
        super::protocol::CallToolResult {
            content: vec![super::protocol::ContentBlock::text(
                serde_json::json!({
                    "symbol": "shared_func",
                    "ambiguous": true,
                    "candidates": [
                        {
                            "qualified_name": "shared_func",
                            "file_path": "src/a.rs",
                            "line": 10,
                            "symbol_ref": {
                                "qualified_name": "shared_func",
                                "file_path": "src/a.rs",
                                "line": 10,
                                "kind": "function",
                                "language": "rust"
                            }
                        },
                        {
                            "qualified_name": "shared_func",
                            "file_path": "src/b.rs",
                            "line": 20,
                            "symbol_ref": {
                                "qualified_name": "shared_func",
                                "file_path": "src/b.rs",
                                "line": 20,
                                "kind": "function",
                                "language": "rust"
                            }
                        }
                    ]
                })
                .to_string(),
            )],
            is_error: Some(false),
        }
    }

    fn form_elicitation_capabilities() -> rmcp::model::ClientCapabilities {
        rmcp::model::ClientCapabilities::builder()
            .enable_elicitation_with(
                rmcp::model::ElicitationCapability::new()
                    .with_form(rmcp::model::FormElicitationCapability::new()),
            )
            .build()
    }

    fn explore_selection_responses(
        action: &str,
        selection: Option<&serde_json::Value>,
    ) -> rmcp::model::InputResponses {
        let mut responses = rmcp::model::InputResponses::new();
        let mut response = serde_json::json!({"action": action});
        if let Some(selection) = selection {
            response["content"] = serde_json::json!({
                "selection": serde_json::to_string(selection).unwrap()
            });
        }
        responses.insert(super::EXPLORE_CANDIDATE_INPUT_ID.into(), response);
        responses
    }

    fn project_path_responses(
        action: &str,
        project_path: Option<&str>,
    ) -> rmcp::model::InputResponses {
        let mut responses = rmcp::model::InputResponses::new();
        let mut response = serde_json::json!({"action": action});
        if let Some(project_path) = project_path {
            response["content"] = serde_json::json!({
                super::PROJECT_PATH_FIELD: project_path
            });
        }
        responses.insert(super::PROJECT_PATH_INPUT_ID.into(), response);
        responses
    }

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
    fn server_supports_legacy_and_current_protocol_versions() {
        let service = super::AtlasMcpService::new_unopened();
        let versions = rmcp::ServerHandler::supported_protocol_versions(&service);

        assert!(versions.contains(&rmcp::model::ProtocolVersion::V_2025_11_25));
        assert!(versions.contains(&rmcp::model::ProtocolVersion::V_2026_07_28));
    }

    #[test]
    fn server_new_is_constructable() {
        // Verify the server struct compiles without Mutex
    }

    #[test]
    fn sep_2322_protocol_gate_requires_a_valid_date_version_at_the_boundary() {
        fn version(value: &str) -> rmcp::model::ProtocolVersion {
            serde_json::from_value(serde_json::json!(value)).expect("protocol version parses")
        }

        assert!(!super::protocol_supports_sep_2322(None));
        assert!(!super::protocol_supports_sep_2322(Some(&version(
            "2026-07-27"
        ))));
        assert!(super::protocol_supports_sep_2322(Some(&version(
            "2026-07-28"
        ))));
        assert!(super::protocol_supports_sep_2322(Some(&version(
            "2027-01-01"
        ))));
        assert!(super::protocol_supports_sep_2322(Some(&version(
            "2028-02-29"
        ))));
        assert!(super::protocol_supports_sep_2322(Some(&version(
            "2400-02-29"
        ))));
        for malformed in [
            "2026-7-28",
            "2026-07-28-rc.1",
            "2026-13-01",
            "2026-09-31",
            "2026-11-31",
            "2027-02-29",
            "2028-02-30",
            "2100-02-29",
            "0000-12-31",
        ] {
            assert!(
                !super::protocol_supports_sep_2322(Some(&version(malformed))),
                "malformed or non-date version must not enable SEP-2322: {malformed}"
            );
        }
    }

    #[test]
    fn query_tasks_require_modern_protocol_and_client_extension() {
        let tasks = rmcp::model::ClientCapabilities::builder()
            .enable_tasks()
            .build();

        assert!(!super::AtlasMcpService::query_tasks_supported(
            None,
            Some(&tasks),
        ));
        assert!(!super::AtlasMcpService::query_tasks_supported(
            Some(&rmcp::model::ProtocolVersion::V_2025_11_25),
            Some(&tasks),
        ));
        assert!(!super::AtlasMcpService::query_tasks_supported(
            Some(&rmcp::model::ProtocolVersion::V_2026_07_28),
            None,
        ));
        assert!(super::AtlasMcpService::query_tasks_supported(
            Some(&rmcp::model::ProtocolVersion::V_2026_07_28),
            Some(&tasks),
        ));
        assert!(super::AtlasMcpService::query_task_supported_for_tool(
            "search", true, false,
        ));
        assert!(!super::AtlasMcpService::query_task_supported_for_tool(
            super::EXPLORE_TOOL_NAME,
            true,
            false,
        ));
        assert!(super::AtlasMcpService::query_task_supported_for_tool(
            super::EXPLORE_TOOL_NAME,
            true,
            true,
        ));

        let service = super::AtlasMcpService::new_unopened();
        let info = rmcp::ServerHandler::get_info(&service);
        assert!(info.capabilities.supports_tasks());
    }

    #[test]
    fn retryable_query_detection_is_strict_and_polling_is_bounded() {
        let valid = retryable_query_result("q_valid", 5_000);
        assert_eq!(
            super::AtlasMcpService::retryable_query(&valid),
            Some(super::RetryableQuery {
                query_id: "q_valid".into(),
                retry_after_ms: 5_000,
            })
        );
        assert_eq!(
            super::AtlasMcpService::bounded_query_task_poll_ms(1),
            super::MIN_QUERY_TASK_POLL_MS
        );
        assert_eq!(
            super::AtlasMcpService::bounded_query_task_poll_ms(u64::MAX),
            super::MAX_QUERY_TASK_POLL_MS
        );

        let malformed = [
            serde_json::json!({"query_id": "q", "analysis": {"retry_after_ms": 0}}),
            serde_json::json!({"query_id": "", "analysis": {"retry_after_ms": 500}}),
            serde_json::json!({"query_id": "q", "analysis": {"retry_after_ms": "500"}}),
            serde_json::json!({"query_id": "q"}),
        ];
        for body in malformed {
            let result = super::protocol::CallToolResult {
                content: vec![super::protocol::ContentBlock::text(body.to_string())],
                is_error: Some(false),
            };
            assert!(super::AtlasMcpService::retryable_query(&result).is_none());
        }

        let mut error = retryable_query_result("q_error", 500);
        error.is_error = Some(true);
        assert!(super::AtlasMcpService::retryable_query(&error).is_none());
        let multiple = super::protocol::CallToolResult {
            content: vec![
                super::protocol::ContentBlock::text("{}"),
                super::protocol::ContentBlock::text("{}"),
            ],
            is_error: Some(false),
        };
        assert!(super::AtlasMcpService::retryable_query(&multiple).is_none());
    }

    #[test]
    fn pinned_query_replay_keeps_the_original_project_after_switch() {
        let first_root = tempfile::tempdir().expect("first project root");
        let router = task_test_router(first_root.path());
        store_search_snapshot(&router, "q_pinned");
        let pinned = router
            .pin_query_replay("q_pinned")
            .expect("snapshot should bind to its project");

        let second_root = tempfile::tempdir().expect("second project root");
        let second_store = atlas_engine::Store::open_in_memory().expect("second project store");
        second_store.init_schema().expect("second project schema");
        let second = super::tools::active_project::ActiveProject::new(
            std::sync::Arc::new(second_store),
            second_root.path().to_path_buf(),
        )
        .expect("second active project");
        router.project.replace(second);

        assert!(router.pin_query_replay("q_pinned").is_none());
        assert_eq!(pinned.tool_name, "search");
        assert_eq!(pinned.tool_args["query"], "task_probe_no_match");
        let result = pinned.router.call_tool(
            &super::tools::ToolCallContext::empty(),
            "resume_query",
            &serde_json::json!({"query_id": "q_pinned"}),
        );
        let text = match result.content.first().expect("resume result") {
            super::protocol::ContentBlock::Text { text } => text,
        };
        assert!(!text.contains("query not found or expired"), "{text}");
    }

    #[test]
    fn pinned_project_scope_keeps_initial_call_and_task_preparation_on_one_project() {
        let first_root = tempfile::tempdir().expect("initial project root");
        let router = task_test_router(first_root.path());
        insert_task_symbol(&router, "src/a.ts");
        insert_task_symbol(&router, "src/b.ts");
        let scoped = router.pin_project_scope();

        let second_root = tempfile::tempdir().expect("replacement project root");
        let second_store =
            atlas_engine::Store::open_in_memory().expect("replacement project store");
        second_store
            .init_schema()
            .expect("replacement project schema");
        let second = super::tools::active_project::ActiveProject::new(
            std::sync::Arc::new(second_store),
            second_root.path().to_path_buf(),
        )
        .expect("replacement active project");
        router.project.replace(second);

        let result = scoped.call_tool(
            &super::tools::ToolCallContext::empty(),
            super::EXPLORE_TOOL_NAME,
            &serde_json::json!({
                "symbol": "shared_func",
                "source_mode": "none",
                "relation_limit": 7
            }),
        );
        let text = match result.content.first().expect("scoped explore result") {
            super::protocol::ContentBlock::Text { text } => text,
        };
        let body: serde_json::Value = serde_json::from_str(text).expect("scoped result is JSON");
        assert_eq!(body["ambiguous"], true);
        assert_eq!(body["candidates"].as_array().map(Vec::len), Some(2));

        store_explore_snapshot(&scoped, "q_scoped_initial");
        assert!(router.pin_query_replay("q_scoped_initial").is_none());
        let prepared = super::AtlasMcpService::prepare_query_task(
            &scoped,
            super::EXPLORE_TOOL_NAME,
            true,
            &retryable_query_result("q_scoped_initial", 1),
        )
        .expect("task preparation should bind the same scoped project");
        assert_eq!(prepared.replay.tool_name, super::EXPLORE_TOOL_NAME);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn query_task_conversion_requires_form_for_explore_and_a_matching_snapshot() {
        let root = tempfile::tempdir().expect("task conversion project root");
        let router = task_test_router(root.path());
        store_explore_snapshot(&router, "q_explore_retry");
        let manager = rmcp::task_manager::TaskManager::new();

        assert!(
            super::AtlasMcpService::maybe_spawn_query_task(
                &manager,
                &router,
                std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                super::EXPLORE_TOOL_NAME,
                true,
                false,
                &retryable_query_result("q_explore_retry", 500),
            )
            .is_none(),
            "explore without form support must retain its query_id flow"
        );
        assert!(
            super::AtlasMcpService::maybe_spawn_query_task(
                &manager,
                &router,
                std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                "search",
                false,
                false,
                &retryable_query_result("q_explore_retry", 500),
            )
            .is_none(),
            "unsupported clients keep the retry ticket"
        );
        assert!(
            super::AtlasMcpService::maybe_spawn_query_task(
                &manager,
                &router,
                std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                "search",
                true,
                false,
                &retryable_query_result("q_missing_snapshot", 500),
            )
            .is_none(),
            "unbound query IDs must not create tasks"
        );
        assert!(
            super::AtlasMcpService::maybe_spawn_query_task(
                &manager,
                &router,
                std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                "search",
                true,
                false,
                &retryable_query_result("q_explore_retry", 500),
            )
            .is_none(),
            "a snapshot for another tool must not be taskified"
        );
        assert!(
            super::AtlasMcpService::maybe_spawn_query_task(
                &manager,
                &router,
                std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                super::EXPLORE_TOOL_NAME,
                true,
                true,
                &retryable_query_result("q_explore_retry", 500),
            )
            .is_some(),
            "explore requires both Tasks and form support"
        );
        manager.shutdown();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explore_query_task_accepts_candidate_and_completes_on_the_pinned_project() {
        let root = tempfile::tempdir().expect("explore task project root");
        let router = task_test_router(root.path());
        insert_task_symbol(&router, "src/a.ts");
        insert_task_symbol(&router, "src/b.ts");
        store_explore_snapshot(&router, "q_explore_accept");
        let manager = rmcp::task_manager::TaskManager::new();
        let blocking_gate = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let response = super::AtlasMcpService::spawn_query_task(
            &manager,
            &router,
            std::sync::Arc::clone(&blocking_gate),
            super::EXPLORE_TOOL_NAME,
            true,
            &retryable_query_result("q_explore_accept", 1),
        )
        .expect("form-capable explore retry should become a task");
        let rmcp::model::CallToolResponse::Task(create) = response else {
            panic!("explore retry must return CreateTaskResult");
        };

        let input = wait_for_task_status(
            &manager,
            &create.task.task_id,
            rmcp::model::TaskStatus::InputRequired,
        )
        .await;
        assert_eq!(
            blocking_gate.available_permits(),
            1,
            "waiting for task input must not retain the shared blocking permit"
        );
        let selection = task_candidate_selection(&input, 1);

        let second_root = tempfile::tempdir().expect("replacement project root");
        let second_store =
            atlas_engine::Store::open_in_memory().expect("replacement project store");
        second_store
            .init_schema()
            .expect("replacement project schema");
        let second = super::tools::active_project::ActiveProject::new(
            std::sync::Arc::new(second_store),
            second_root.path().to_path_buf(),
        )
        .expect("replacement active project");
        router.project.replace(second);

        manager
            .update_task(
                &create.task.task_id,
                [(
                    super::EXPLORE_CANDIDATE_INPUT_ID.to_string(),
                    serde_json::json!({
                        "action": "accept",
                        "content": {super::EXPLORE_CANDIDATE_FIELD: selection}
                    }),
                )],
            )
            .expect("accepted candidate update is delivered");

        let completed = wait_for_task_status(
            &manager,
            &create.task.task_id,
            rmcp::model::TaskStatus::Completed,
        )
        .await;
        let body = completed_task_body(completed);
        assert_ne!(body.get("ambiguous"), Some(&serde_json::Value::Bool(true)));
        assert_eq!(body["subject"]["file"], "src/b.ts");

        manager
            .update_task(
                &create.task.task_id,
                [(
                    super::EXPLORE_CANDIDATE_INPUT_ID.to_string(),
                    serde_json::json!({"action": "cancel"}),
                )],
            )
            .expect("late or duplicate input is ignored for a known task");
        assert_eq!(
            manager
                .get_task(&create.task.task_id)
                .expect("completed task remains observable")
                .status(),
            rmcp::model::TaskStatus::Completed
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_explore_tasks_keep_the_fixed_input_id_task_local() {
        let root = tempfile::tempdir().expect("concurrent explore task project root");
        let router = task_test_router(root.path());
        insert_task_symbol(&router, "src/a.ts");
        insert_task_symbol(&router, "src/b.ts");
        store_explore_snapshot(&router, "q_explore_concurrent_a");
        store_explore_snapshot(&router, "q_explore_concurrent_b");
        let manager = rmcp::task_manager::TaskManager::new();
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(2));

        let mut task_ids = Vec::new();
        for query_id in ["q_explore_concurrent_a", "q_explore_concurrent_b"] {
            let response = super::AtlasMcpService::spawn_query_task(
                &manager,
                &router,
                std::sync::Arc::clone(&gate),
                super::EXPLORE_TOOL_NAME,
                true,
                &retryable_query_result(query_id, 1),
            )
            .expect("concurrent explore retry should become a task");
            let rmcp::model::CallToolResponse::Task(create) = response else {
                panic!("concurrent explore retry must return CreateTaskResult");
            };
            task_ids.push(create.task.task_id);
        }

        let first = wait_for_task_status(
            &manager,
            &task_ids[0],
            rmcp::model::TaskStatus::InputRequired,
        )
        .await;
        let second = wait_for_task_status(
            &manager,
            &task_ids[1],
            rmcp::model::TaskStatus::InputRequired,
        )
        .await;
        let first_selection = task_candidate_selection(&first, 0);
        let second_selection = task_candidate_selection(&second, 1);
        for (task_id, selection) in [
            (&task_ids[0], first_selection),
            (&task_ids[1], second_selection),
        ] {
            manager
                .update_task(
                    task_id,
                    [(
                        super::EXPLORE_CANDIDATE_INPUT_ID.to_string(),
                        serde_json::json!({
                            "action": "accept",
                            "content": {super::EXPLORE_CANDIDATE_FIELD: selection}
                        }),
                    )],
                )
                .expect("task-local candidate response is delivered");
        }

        let first_body = completed_task_body(
            wait_for_task_status(&manager, &task_ids[0], rmcp::model::TaskStatus::Completed).await,
        );
        let second_body = completed_task_body(
            wait_for_task_status(&manager, &task_ids[1], rmcp::model::TaskStatus::Completed).await,
        );
        assert_eq!(first_body["subject"]["file"], "src/a.ts");
        assert_eq!(second_body["subject"]["file"], "src/b.ts");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn accepted_cold_selector_retries_inside_the_same_explore_task() {
        let root = tempfile::tempdir().expect("cold selected explore task project root");
        std::fs::write(
            root.path().join("target.c"),
            "void unscoped_target(void) {}\n",
        )
        .expect("cold selected source file");
        let router = task_test_router(root.path());
        insert_task_symbol(&router, "src/a.ts");
        insert_task_symbol(&router, "src/b.ts");
        store_explore_snapshot(&router, "q_explore_selected_retry");
        let manager = rmcp::task_manager::TaskManager::new();
        let response = super::AtlasMcpService::spawn_query_task(
            &manager,
            &router,
            std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            super::EXPLORE_TOOL_NAME,
            true,
            &retryable_query_result("q_explore_selected_retry", 1),
        )
        .expect("cold selected explore retry should become a task");
        let rmcp::model::CallToolResponse::Task(create) = response else {
            panic!("cold selected explore retry must return CreateTaskResult");
        };
        let original_task_id = create.task.task_id;
        wait_for_task_status(
            &manager,
            &original_task_id,
            rmcp::model::TaskStatus::InputRequired,
        )
        .await;
        manager
            .update_task(
                &original_task_id,
                [(
                    super::EXPLORE_CANDIDATE_INPUT_ID.to_string(),
                    serde_json::json!({
                        "action": "accept",
                        "content": {
                            super::EXPLORE_CANDIDATE_FIELD:
                                serde_json::json!({"qualified_name": "unscoped_target"}).to_string()
                        }
                    }),
                )],
            )
            .expect("cold selector response is delivered");

        let mut saw_working = false;
        let mut completed = None;
        for _ in 0..500 {
            let detailed = manager
                .get_task(&original_task_id)
                .expect("selected retry task remains observable");
            match detailed.status() {
                rmcp::model::TaskStatus::Working => {
                    saw_working = true;
                    assert!(matches!(
                        detailed.payload,
                        rmcp::model::TaskPayload::Working
                    ));
                }
                rmcp::model::TaskStatus::Completed => {
                    completed = Some(detailed);
                    break;
                }
                rmcp::model::TaskStatus::InputRequired => {
                    panic!("accepted retry must not publish a second candidate input")
                }
                status => panic!("selected retry task reached unexpected status {status:?}"),
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            saw_working,
            "accepted cold selector should continue within the task"
        );
        assert_eq!(manager.running_task_count(), 0);
        let body = completed_task_body(completed.expect("selected retry task completes"));
        assert_eq!(body["subject"]["qualifiedName"], "unscoped_target");
        assert!(
            body["resumed_from"].as_str().is_some(),
            "terminal result must come from the internal retry replay: {body:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explore_query_task_decline_completes_the_original_candidate_result() {
        let root = tempfile::tempdir().expect("declined explore task project root");
        let router = task_test_router(root.path());
        insert_task_symbol(&router, "src/a.ts");
        insert_task_symbol(&router, "src/b.ts");
        store_explore_snapshot(&router, "q_explore_decline");
        let manager = rmcp::task_manager::TaskManager::new();
        let response = super::AtlasMcpService::spawn_query_task(
            &manager,
            &router,
            std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            super::EXPLORE_TOOL_NAME,
            true,
            &retryable_query_result("q_explore_decline", 1),
        )
        .expect("explore retry should become a task");
        let rmcp::model::CallToolResponse::Task(create) = response else {
            panic!("explore retry must return CreateTaskResult");
        };
        wait_for_task_status(
            &manager,
            &create.task.task_id,
            rmcp::model::TaskStatus::InputRequired,
        )
        .await;
        manager
            .update_task(
                &create.task.task_id,
                [(
                    super::EXPLORE_CANDIDATE_INPUT_ID.to_string(),
                    serde_json::json!({"action": "decline"}),
                )],
            )
            .expect("declined candidate update is delivered");

        let completed = wait_for_task_status(
            &manager,
            &create.task.task_id,
            rmcp::model::TaskStatus::Completed,
        )
        .await;
        let body = completed_task_body(completed);
        assert_eq!(body["ambiguous"], true);
        assert_eq!(body["candidates"].as_array().map(Vec::len), Some(2));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explore_query_task_rejects_malformed_input_and_cancels_pending_input() {
        for (query_id, cancel) in [("q_explore_malformed", false), ("q_explore_cancel", true)] {
            let root = tempfile::tempdir().expect("explore task input project root");
            let router = task_test_router(root.path());
            insert_task_symbol(&router, "src/a.ts");
            insert_task_symbol(&router, "src/b.ts");
            store_explore_snapshot(&router, query_id);
            let manager = rmcp::task_manager::TaskManager::new();
            let response = super::AtlasMcpService::spawn_query_task(
                &manager,
                &router,
                std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                super::EXPLORE_TOOL_NAME,
                true,
                &retryable_query_result(query_id, 1),
            )
            .expect("explore retry should become a task");
            let rmcp::model::CallToolResponse::Task(create) = response else {
                panic!("explore retry must return CreateTaskResult");
            };
            wait_for_task_status(
                &manager,
                &create.task.task_id,
                rmcp::model::TaskStatus::InputRequired,
            )
            .await;

            if cancel {
                manager
                    .cancel_task(&create.task.task_id)
                    .expect("input-required task cancellation is acknowledged");
                wait_for_task_status(
                    &manager,
                    &create.task.task_id,
                    rmcp::model::TaskStatus::Cancelled,
                )
                .await;
            } else {
                manager
                    .update_task(
                        &create.task.task_id,
                        [(
                            super::EXPLORE_CANDIDATE_INPUT_ID.to_string(),
                            serde_json::json!({"action": "accept"}),
                        )],
                    )
                    .expect("malformed update is delivered to the task");
                let failed = wait_for_task_status(
                    &manager,
                    &create.task.task_id,
                    rmcp::model::TaskStatus::Failed,
                )
                .await;
                let rmcp::model::TaskPayload::Failed { error } = failed.payload else {
                    panic!("malformed candidate response must fail the task");
                };
                assert_eq!(
                    error["code"],
                    rmcp::model::ErrorCode::INVALID_PARAMS.0,
                    "{error:?}"
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explore_query_task_input_deadline_and_single_round_are_enforced() {
        let root = tempfile::tempdir().expect("explore task deadline project root");
        let router = std::sync::Arc::new(task_test_router(root.path()));
        insert_task_symbol(&router, "src/a.ts");
        insert_task_symbol(&router, "src/b.ts");
        store_explore_snapshot(&router, "q_explore_deadline");
        let pinned = router
            .pin_query_replay("q_explore_deadline")
            .expect("explore deadline snapshot should bind");
        let manager = rmcp::task_manager::TaskManager::new();
        let task = manager.spawn(
            rmcp::task_manager::TaskOptions::new().with_ttl_ms(2_000),
            move |context| {
                Box::pin(super::AtlasMcpService::run_query_task_with_ttl(
                    context,
                    super::QueryTaskRun {
                        replay_router: std::sync::Arc::new(pinned.router),
                        blocking_gate: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
                        retry: super::RetryableQuery {
                            query_id: "q_explore_deadline".into(),
                            retry_after_ms: 1,
                        },
                        tool_name: pinned.tool_name,
                        tool_args: pinned.tool_args,
                        allow_candidate_input: true,
                    },
                    std::time::Duration::from_millis(600),
                ))
            },
        );
        wait_for_task_status(
            &manager,
            &task.task_id,
            rmcp::model::TaskStatus::InputRequired,
        )
        .await;
        let failed =
            wait_for_task_status(&manager, &task.task_id, rmcp::model::TaskStatus::Failed).await;
        let failed_wire = serde_json::to_value(&failed).expect("failed task serializes");
        assert!(
            failed_wire.get("inputRequests").is_none(),
            "terminal task must not retain expired input requests: {failed_wire:?}"
        );
        let rmcp::model::TaskPayload::Failed { error } = failed.payload else {
            panic!("unanswered candidate input must fail at the hard deadline");
        };
        assert!(
            error["message"]
                .as_str()
                .is_some_and(|message| message.contains("expired")),
            "{error:?}"
        );

        let root = tempfile::tempdir().expect("single-round explore task project root");
        let router = task_test_router(root.path());
        insert_task_symbol(&router, "src/a.ts");
        insert_task_symbol(&router, "src/b.ts");
        store_explore_snapshot(&router, "q_explore_single_round");
        let manager = rmcp::task_manager::TaskManager::new();
        let response = super::AtlasMcpService::spawn_query_task(
            &manager,
            &router,
            std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            super::EXPLORE_TOOL_NAME,
            true,
            &retryable_query_result("q_explore_single_round", 1),
        )
        .expect("explore retry should become a task");
        let rmcp::model::CallToolResponse::Task(create) = response else {
            panic!("explore retry must return CreateTaskResult");
        };
        wait_for_task_status(
            &manager,
            &create.task.task_id,
            rmcp::model::TaskStatus::InputRequired,
        )
        .await;
        manager
            .update_task(
                &create.task.task_id,
                [(
                    super::EXPLORE_CANDIDATE_INPUT_ID.to_string(),
                    serde_json::json!({
                        "action": "accept",
                        "content": {
                            super::EXPLORE_CANDIDATE_FIELD:
                                serde_json::json!({"qualified_name": "shared_func"}).to_string()
                        }
                    }),
                )],
            )
            .expect("enum-outside ordinary selector is delivered");
        let completed = wait_for_task_status(
            &manager,
            &create.task.task_id,
            rmcp::model::TaskStatus::Completed,
        )
        .await;
        let body = completed_task_body(completed);
        assert_eq!(body["ambiguous"], true);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn query_task_replays_to_a_standard_completed_tool_result() {
        let root = tempfile::tempdir().expect("task project root");
        let router = task_test_router(root.path());
        store_search_snapshot(&router, "q_task_complete");
        let manager = rmcp::task_manager::TaskManager::new();
        let response = super::AtlasMcpService::spawn_query_task(
            &manager,
            &router,
            std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            "search",
            false,
            &retryable_query_result("q_task_complete", 1),
        )
        .expect("retryable query should become a task");
        let rmcp::model::CallToolResponse::Task(create) = response else {
            panic!("retryable query must return CreateTaskResult");
        };
        assert_eq!(create.task.ttl_ms, Some(super::QUERY_TASK_TTL_MS));
        assert_eq!(
            create.task.poll_interval_ms,
            Some(super::MIN_QUERY_TASK_POLL_MS)
        );
        let wire = serde_json::to_value(&create).expect("CreateTaskResult serializes");
        assert_eq!(wire["resultType"], "task");
        assert_eq!(wire["taskId"], create.task.task_id);
        assert_eq!(wire["status"], "working");

        let mut terminal = None;
        for _ in 0..100 {
            let detailed = manager
                .get_task(&create.task.task_id)
                .expect("task remains observable");
            if detailed.status().is_terminal() {
                terminal = Some(detailed);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let terminal = terminal.expect("query task should complete");
        assert_eq!(terminal.status(), rmcp::model::TaskStatus::Completed);
        match terminal.payload {
            rmcp::model::TaskPayload::Completed { result } => {
                let wire = serde_json::Value::Object(result);
                assert!(
                    wire["content"]
                        .as_array()
                        .is_some_and(|items| !items.is_empty())
                );
            }
            other => panic!("expected completed task payload, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn query_task_keeps_tool_errors_as_completed_results() {
        let root = tempfile::tempdir().expect("task error project root");
        let router = task_test_router(root.path());
        router.store_snapshot(super::tools::query_snapshot::QuerySnapshot {
            query_id: "q_task_tool_error".into(),
            tool_name: "unsupported_for_resume".into(),
            tool_args: serde_json::json!({}),
            focus_result: None,
            created_at: std::time::Instant::now(),
            status: super::tools::query_snapshot::QueryStatus::Retryable,
        });
        let manager = rmcp::task_manager::TaskManager::new();
        let response = super::AtlasMcpService::spawn_query_task(
            &manager,
            &router,
            std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            "unsupported_for_resume",
            false,
            &retryable_query_result("q_task_tool_error", 1),
        )
        .expect("retryable query should become a task");
        let rmcp::model::CallToolResponse::Task(create) = response else {
            panic!("retryable query must return CreateTaskResult");
        };

        let mut terminal = None;
        for _ in 0..100 {
            let detailed = manager
                .get_task(&create.task.task_id)
                .expect("task remains observable");
            if detailed.status().is_terminal() {
                terminal = Some(detailed);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        let terminal = terminal.expect("tool-error task should settle");
        assert_eq!(terminal.status(), rmcp::model::TaskStatus::Completed);
        match terminal.payload {
            rmcp::model::TaskPayload::Completed { result } => {
                assert_eq!(result.get("isError"), Some(&serde_json::Value::Bool(true)));
            }
            other => panic!("expected completed task payload, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn query_task_has_an_internal_deadline_without_client_polling() {
        let root = tempfile::tempdir().expect("task deadline project root");
        let router = std::sync::Arc::new(task_test_router(root.path()));
        store_search_snapshot(&router, "q_task_deadline");
        let replay = std::sync::Arc::new(
            router
                .pin_query_replay("q_task_deadline")
                .expect("deadline snapshot should bind")
                .router,
        );
        let manager = rmcp::task_manager::TaskManager::new();
        let task = manager.spawn(
            rmcp::task_manager::TaskOptions::new().with_ttl_ms(1_000),
            move |context| {
                Box::pin(super::AtlasMcpService::run_query_task_with_ttl(
                    context,
                    super::QueryTaskRun {
                        replay_router: replay,
                        blocking_gate: std::sync::Arc::new(tokio::sync::Semaphore::new(0)),
                        retry: super::RetryableQuery {
                            query_id: "q_task_deadline".into(),
                            retry_after_ms: 1,
                        },
                        tool_name: "search".into(),
                        tool_args: serde_json::json!({"query": "task_probe_no_match"}),
                        allow_candidate_input: false,
                    },
                    std::time::Duration::from_millis(30),
                ))
            },
        );

        // Do not poll during the operation: the task's own deadline must settle
        // it rather than relying on TaskManager's opportunistic TTL sweep.
        tokio::time::sleep(std::time::Duration::from_millis(80)).await;
        let detailed = manager
            .get_task(&task.task_id)
            .expect("expired task remains observable");
        assert_eq!(detailed.status(), rmcp::model::TaskStatus::Failed);
        match detailed.payload {
            rmcp::model::TaskPayload::Failed { error } => assert!(
                error["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("expired")),
                "{error:?}"
            ),
            other => panic!("expected failed task payload, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn query_task_deadline_settles_while_a_blocking_worker_is_still_running() {
        let manager = rmcp::task_manager::TaskManager::new();
        let gate = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
        let task_gate = std::sync::Arc::clone(&gate);
        let task = manager.spawn(
            rmcp::task_manager::TaskOptions::new().with_ttl_ms(1_000),
            move |context| {
                Box::pin(async move {
                    let permit = task_gate
                        .acquire_owned()
                        .await
                        .expect("blocking worker gate remains open");
                    let worker = tokio::task::spawn_blocking(move || {
                        let _permit = permit;
                        std::thread::sleep(std::time::Duration::from_millis(150));
                        super::protocol::CallToolResult {
                            content: vec![super::protocol::ContentBlock::text("{}")],
                            is_error: Some(false),
                        }
                    });
                    super::AtlasMcpService::await_query_task_worker(
                        &context,
                        tokio::time::Instant::now() + std::time::Duration::from_millis(30),
                        worker,
                    )
                    .await
                    .map(super::AtlasMcpService::to_rmcp_result)
                })
            },
        );

        // Do not poll until after Atlas's own deadline. The blocking worker is
        // intentionally still running, so task settlement cannot depend on its join.
        tokio::time::sleep(std::time::Duration::from_millis(70)).await;
        let detailed = manager
            .get_task(&task.task_id)
            .expect("expired blocking task remains observable");
        assert_eq!(detailed.status(), rmcp::model::TaskStatus::Failed);
        assert_eq!(
            gate.available_permits(),
            0,
            "a running synchronous core call remains cooperative and keeps its permit"
        );

        tokio::time::sleep(std::time::Duration::from_millis(120)).await;
        assert_eq!(
            gate.available_permits(),
            1,
            "detached blocking work eventually releases its permit"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_the_service_shuts_down_and_forgets_running_tasks() {
        let service = super::AtlasMcpService::new_unopened();
        let manager = service.task_manager.clone();
        let task = service.task_manager.spawn(
            rmcp::task_manager::TaskOptions::new().with_ttl_ms(None),
            |context| {
                Box::pin(async move {
                    context.cancelled().await;
                    Err(rmcp::task_manager::TaskExit::Cancelled)
                })
            },
        );
        assert_eq!(manager.running_task_count(), 1);
        drop(service);
        assert_eq!(manager.running_task_count(), 0);
        assert_eq!(
            manager
                .get_task(&task.task_id)
                .expect_err("shutdown removes task state")
                .code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn query_task_cancellation_stops_the_waiter_and_unknown_ids_are_rejected() {
        let root = tempfile::tempdir().expect("task cancellation project root");
        let router = task_test_router(root.path());
        store_search_snapshot(&router, "q_task_cancel");
        let manager = rmcp::task_manager::TaskManager::new();
        let response = super::AtlasMcpService::spawn_query_task(
            &manager,
            &router,
            std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
            "search",
            false,
            &retryable_query_result("q_task_cancel", super::MAX_QUERY_TASK_POLL_MS),
        )
        .expect("retryable query should become a cancellable task");
        let rmcp::model::CallToolResponse::Task(create) = response else {
            panic!("retryable query must return CreateTaskResult");
        };
        manager
            .update_task(
                &create.task.task_id,
                [(
                    "unused".to_string(),
                    serde_json::json!({"action": "cancel"}),
                )],
            )
            .expect("known task accepts an update even without pending inputs");
        manager
            .cancel_task(&create.task.task_id)
            .expect("known task cancellation is acknowledged");

        let mut cancelled = false;
        for _ in 0..100 {
            let detailed = manager
                .get_task(&create.task.task_id)
                .expect("cancelled task remains observable");
            if detailed.status() == rmcp::model::TaskStatus::Cancelled {
                cancelled = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(cancelled, "task should settle as cancelled");

        assert_eq!(
            manager
                .get_task("missing")
                .expect_err("unknown task must fail")
                .code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );
        assert_eq!(
            manager
                .update_task("missing", std::iter::empty::<(String, serde_json::Value)>(),)
                .expect_err("unknown task update must fail")
                .code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );
        assert_eq!(
            manager
                .cancel_task("missing")
                .expect_err("unknown task cancellation must fail")
                .code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn task_methods_round_trip_over_rmcp_and_legacy_protocol_is_rejected() {
        let service = super::AtlasMcpService::new_unopened();
        let task = service.task_manager.spawn(
            rmcp::task_manager::TaskOptions::new().with_poll_interval_ms(10),
            |context| {
                Box::pin(async move {
                    context.cancelled().await;
                    Err(rmcp::task_manager::TaskExit::Cancelled)
                })
            },
        );
        let task_id = task.task_id.clone();
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            service.serve(server_transport).await?.waiting().await?;
            anyhow::Ok(())
        });
        let client = VersionedTasksClient {
            protocol_version: rmcp::model::ProtocolVersion::V_2026_07_28,
        }
        .serve(client_transport)
        .await
        .expect("modern tasks client connects");

        let initial = client
            .peer()
            .get_task(rmcp::model::GetTaskParams::new(task_id.clone()))
            .await
            .expect("tasks/get returns the known task");
        assert_eq!(initial.task.status(), rmcp::model::TaskStatus::Working);
        client
            .peer()
            .update_task(rmcp::model::UpdateTaskParams::new(
                task_id.clone(),
                rmcp::model::InputResponses::new(),
            ))
            .await
            .expect("tasks/update is acknowledged");
        client
            .peer()
            .cancel_task(rmcp::model::CancelTaskParams::new(task_id.clone()))
            .await
            .expect("tasks/cancel is acknowledged");

        let mut cancelled = false;
        for _ in 0..100 {
            let current = client
                .peer()
                .get_task(rmcp::model::GetTaskParams::new(task_id.clone()))
                .await
                .expect("cancelled task remains observable");
            if current.task.status() == rmcp::model::TaskStatus::Cancelled {
                cancelled = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(cancelled, "wire-visible task should settle as cancelled");
        client.cancel().await.expect("modern client shutdown");
        server
            .await
            .expect("modern server join")
            .expect("modern server");

        let service = super::AtlasMcpService::new_unopened();
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            service.serve(server_transport).await?.waiting().await?;
            anyhow::Ok(())
        });
        let client = VersionedTasksClient {
            protocol_version: rmcp::model::ProtocolVersion::V_2025_11_25,
        }
        .serve(client_transport)
        .await
        .expect("legacy client connects");
        let error = client
            .peer()
            .get_task(rmcp::model::GetTaskParams::new("not-visible-to-legacy"))
            .await
            .expect_err("legacy protocol must not enable Tasks Extension methods");
        match error {
            rmcp::ServiceError::McpError(error) => {
                assert_eq!(error.code, rmcp::model::ErrorCode::METHOD_NOT_FOUND);
            }
            other => panic!("expected MCP method-not-found error, got {other:?}"),
        }
        client.cancel().await.expect("legacy client shutdown");
        server
            .await
            .expect("legacy server join")
            .expect("legacy server");

        let service = super::AtlasMcpService::new_unopened();
        let (server_transport, client_transport) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            service.serve(server_transport).await?.waiting().await?;
            anyhow::Ok(())
        });
        let client = ().serve(client_transport).await.expect("plain client connects");
        let error = client
            .peer()
            .get_task(rmcp::model::GetTaskParams::new("not-declared"))
            .await
            .expect_err("client without Tasks Extension cannot use tasks/get");
        match error {
            rmcp::ServiceError::McpError(error) => {
                assert_eq!(
                    error.code,
                    rmcp::model::ErrorCode::MISSING_REQUIRED_CLIENT_CAPABILITY
                );
            }
            other => panic!("expected missing-capability error, got {other:?}"),
        }
        client.cancel().await.expect("plain client shutdown");
        server
            .await
            .expect("plain server join")
            .expect("plain server");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn explore_task_input_round_trips_over_rmcp_tasks_update() {
        let root = tempfile::tempdir().expect("wire explore task project root");
        let store = atlas_engine::Store::open_in_memory().expect("wire explore task store");
        store.init_schema().expect("wire explore task schema");
        let service =
            super::AtlasMcpService::new(std::sync::Arc::new(store), root.path().to_path_buf());
        insert_task_symbol(service.router.as_ref(), "src/a.ts");
        insert_task_symbol(service.router.as_ref(), "src/b.ts");
        store_explore_snapshot(service.router.as_ref(), "q_explore_wire");
        let response = super::AtlasMcpService::spawn_query_task(
            &service.task_manager,
            service.router.as_ref(),
            std::sync::Arc::clone(&service.blocking_gate),
            super::EXPLORE_TOOL_NAME,
            true,
            &retryable_query_result("q_explore_wire", 1),
        )
        .expect("wire explore retry should become a task");
        let rmcp::model::CallToolResponse::Task(create) = response else {
            panic!("wire explore retry must return CreateTaskResult");
        };
        let task_id = create.task.task_id;

        let (server_transport, client_transport) = tokio::io::duplex(16_384);
        let server = tokio::spawn(async move {
            service.serve(server_transport).await?.waiting().await?;
            anyhow::Ok(())
        });
        let client = TaskInputClient
            .serve(client_transport)
            .await
            .expect("task-input client connects");

        let mut input_required = None;
        for _ in 0..200 {
            let current = client
                .peer()
                .get_task(rmcp::model::GetTaskParams::new(task_id.clone()))
                .await
                .expect("tasks/get returns explore task");
            if current.task.status() == rmcp::model::TaskStatus::InputRequired {
                input_required = Some(current.task);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let input_required = input_required.expect("wire task reaches input_required");
        let wire = serde_json::to_value(&input_required).expect("detailed task serializes");
        assert_eq!(wire["status"], "input_required");
        assert_eq!(
            wire["inputRequests"][super::EXPLORE_CANDIDATE_INPUT_ID]["method"],
            "elicitation/create"
        );
        let selection = task_candidate_selection(&input_required, 0);
        let mut responses = rmcp::model::InputResponses::new();
        responses.insert(
            super::EXPLORE_CANDIDATE_INPUT_ID.into(),
            serde_json::json!({
                "action": "accept",
                "content": {super::EXPLORE_CANDIDATE_FIELD: selection}
            }),
        );
        client
            .peer()
            .update_task(rmcp::model::UpdateTaskParams::new(
                task_id.clone(),
                responses,
            ))
            .await
            .expect("wire tasks/update delivers candidate selection");

        let mut completed = None;
        for _ in 0..200 {
            let current = client
                .peer()
                .get_task(rmcp::model::GetTaskParams::new(task_id.clone()))
                .await
                .expect("tasks/get observes selected explore task");
            if current.task.status() == rmcp::model::TaskStatus::Completed {
                completed = Some(current.task);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let body = completed_task_body(completed.expect("wire task completes after update"));
        assert_eq!(body["subject"]["file"], "src/a.ts");

        client.cancel().await.expect("task-input client shutdown");
        server
            .await
            .expect("task-input server join")
            .expect("task-input server");
    }

    #[test]
    fn explore_candidate_mrtr_requires_modern_protocol_and_form_elicitation() {
        let form = form_elicitation_capabilities();
        let empty_elicitation = rmcp::model::ClientCapabilities::builder()
            .enable_elicitation_with(rmcp::model::ElicitationCapability::new())
            .build();
        let url_only = rmcp::model::ClientCapabilities::builder()
            .enable_elicitation_with(
                rmcp::model::ElicitationCapability::new()
                    .with_url(rmcp::model::UrlElicitationCapability::new()),
            )
            .build();

        assert!(!super::AtlasMcpService::form_mrtr_supported(
            None,
            Some(&form),
        ));
        assert!(!super::AtlasMcpService::form_mrtr_supported(
            Some(&rmcp::model::ProtocolVersion::V_2025_11_25),
            Some(&form),
        ));
        assert!(!super::AtlasMcpService::form_mrtr_supported(
            Some(&rmcp::model::ProtocolVersion::V_2026_07_28),
            None,
        ));
        assert!(!super::AtlasMcpService::form_mrtr_supported(
            Some(&rmcp::model::ProtocolVersion::V_2026_07_28),
            Some(&empty_elicitation),
        ));
        assert!(!super::AtlasMcpService::form_mrtr_supported(
            Some(&rmcp::model::ProtocolVersion::V_2026_07_28),
            Some(&url_only),
        ));
        assert!(super::AtlasMcpService::form_mrtr_supported(
            Some(&rmcp::model::ProtocolVersion::V_2026_07_28),
            Some(&form),
        ));
    }

    #[test]
    fn overlay_delete_confirmation_is_gated_and_has_bounded_wire_shape() {
        let root = tempfile::tempdir().expect("overlay confirmation project root");
        let (service, _) = overlay_test_service(root.path());
        let args = serde_json::json!({
            "action": "delete",
            "rule_id": "rule:example",
            "client_note": "preserved"
        });
        let (request_state, input_required) = issue_overlay_delete_confirmation(
            &service,
            super::DOMAIN_RULES_TOOL_NAME,
            args.clone(),
        );
        uuid::Uuid::parse_str(&request_state).expect("requestState is an opaque UUID handle");
        let wire = serde_json::to_value(input_required).expect("confirmation serializes");
        assert_eq!(wire["resultType"], "input_required");
        assert_eq!(wire["requestState"], request_state);
        let request = &wire["inputRequests"][super::OVERLAY_DELETE_CONFIRMATION_INPUT_ID];
        assert_eq!(request["method"], "elicitation/create");
        assert_eq!(
            request["params"]["requestedSchema"]["required"],
            serde_json::json!([super::OVERLAY_DELETE_CONFIRMATION_FIELD])
        );
        assert_eq!(
            request["params"]["requestedSchema"]["properties"]
                [super::OVERLAY_DELETE_CONFIRMATION_FIELD]["type"],
            "boolean"
        );
        assert_eq!(
            request["params"]["requestedSchema"]["properties"]
                [super::OVERLAY_DELETE_CONFIRMATION_FIELD]["default"],
            false
        );
        let message = request["params"]["message"]
            .as_str()
            .expect("confirmation message");
        assert!(message.contains("rule:example"), "{message}");
        assert!(
            message.contains(&root.path().to_string_lossy().to_string()),
            "{message}"
        );
        assert_eq!(
            service
                .overlay_delete_confirmations
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .len(),
            1
        );

        let legacy = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                args.clone(),
                None,
                None,
                false,
            )
            .expect("legacy call keeps direct semantics");
        assert!(legacy.call.immediate_response.is_none());
        assert!(legacy.execution_router.is_none());

        for invalid in [
            serde_json::json!({"action": "list"}),
            serde_json::json!({"action": "delete"}),
            serde_json::json!({"action": "delete", "rule_id": ""}),
            serde_json::json!({"action": "delete", "rule_id": 7}),
        ] {
            let prepared = service
                .prepare_adapter_tool_call(super::DOMAIN_RULES_TOOL_NAME, invalid, None, None, true)
                .expect("non-confirmable shape stays on the core path");
            assert!(prepared.call.immediate_response.is_none());
        }

        let unopened = super::AtlasMcpService::new_unopened();
        let prepared = unopened
            .prepare_adapter_tool_call(super::DOMAIN_RULES_TOOL_NAME, args, None, None, true)
            .expect("unopened service keeps existing no-project result");
        assert!(prepared.call.immediate_response.is_none());

        let fp_args = serde_json::json!({
            "action": "delete",
            "annotation_id": "fpa:preferred",
            "field_qname": "Ignored.field"
        });
        let (_, fp_required) =
            issue_overlay_delete_confirmation(&service, super::FP_DISPATCHES_TOOL_NAME, fp_args);
        let fp_wire = serde_json::to_value(fp_required).expect("FP confirmation serializes");
        let fp_message = fp_wire["inputRequests"]
            [super::OVERLAY_DELETE_CONFIRMATION_INPUT_ID]["params"]["message"]
            .as_str()
            .expect("FP message");
        assert!(fp_message.contains("fpa:preferred"), "{fp_message}");
        assert!(!fp_message.contains("Ignored.field"), "{fp_message}");
    }

    #[test]
    fn accepted_overlay_delete_is_one_shot_and_pinned_to_the_original_project() {
        let first_root = tempfile::tempdir().expect("first overlay project root");
        let (service, first_store) = overlay_test_service(first_root.path());
        let add_args = serde_json::json!({
            "action": "add",
            "language": "c",
            "rule_kind": "free_fn",
            "pattern": "shared_free"
        });
        let (created, created_error) =
            call_tool_json(&service.router, super::DOMAIN_RULES_TOOL_NAME, &add_args);
        assert!(!created_error, "{created:?}");
        let rule_id = created["rule_id"]
            .as_str()
            .expect("created rule id")
            .to_string();
        let delete_args = serde_json::json!({
            "action": "delete",
            "rule_id": rule_id,
            "client_note": "must remain bound"
        });
        let (request_state, _) = issue_overlay_delete_confirmation(
            &service,
            super::DOMAIN_RULES_TOOL_NAME,
            delete_args.clone(),
        );

        let second_root = tempfile::tempdir().expect("second overlay project root");
        let second_store = std::sync::Arc::new(
            atlas_engine::Store::open_in_memory().expect("second overlay store"),
        );
        second_store.init_schema().expect("second overlay schema");
        service.router.activate_project(
            second_root.path().to_path_buf(),
            std::sync::Arc::clone(&second_store),
        );
        let (second_created, second_error) =
            call_tool_json(&service.router, super::DOMAIN_RULES_TOOL_NAME, &add_args);
        assert!(!second_error, "{second_created:?}");
        assert_eq!(second_created["rule_id"], delete_args["rule_id"]);

        let responses = overlay_confirmation_responses("accept", Some(true));
        let prepared = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                delete_args.clone(),
                Some(&responses),
                Some(&request_state),
                true,
            )
            .expect("accepted confirmation prepares the pinned delete");
        assert!(prepared.call.immediate_response.is_none());
        assert_eq!(prepared.call.args, delete_args);
        let pinned = prepared
            .execution_router
            .expect("accepted confirmation carries a pinned router");
        let (deleted, delete_error) =
            call_tool_json(&pinned, super::DOMAIN_RULES_TOOL_NAME, &prepared.call.args);
        assert!(!delete_error, "{deleted:?}");
        assert_eq!(deleted["deleted"], second_created["rule_id"]);
        assert!(
            first_store
                .list_domain_rules(None, None)
                .expect("first rules")
                .is_empty(),
            "the original project rule is deleted"
        );
        assert_eq!(
            second_store
                .list_domain_rules(None, None)
                .expect("second rules")
                .len(),
            1,
            "the active replacement project remains untouched"
        );

        let replay_error = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                delete_args,
                Some(&responses),
                Some(&request_state),
                true,
            )
            .expect_err("the same confirmation handle is one-shot");
        assert_eq!(replay_error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn declined_and_malformed_overlay_confirmations_never_delete() {
        let root = tempfile::tempdir().expect("declined overlay project root");
        let (service, store) = overlay_test_service(root.path());
        let (created, created_error) = call_tool_json(
            &service.router,
            super::DOMAIN_RULES_TOOL_NAME,
            &serde_json::json!({
                "action": "add",
                "rule_kind": "cleanup_fn",
                "pattern": "keep_me"
            }),
        );
        assert!(!created_error, "{created:?}");
        let args = serde_json::json!({
            "action": "delete",
            "rule_id": created["rule_id"].as_str().expect("rule id")
        });

        for (action, confirmed) in [("decline", None), ("cancel", None), ("accept", Some(false))] {
            let (request_state, _) = issue_overlay_delete_confirmation(
                &service,
                super::DOMAIN_RULES_TOOL_NAME,
                args.clone(),
            );
            let responses = overlay_confirmation_responses(action, confirmed);
            let prepared = service
                .prepare_adapter_tool_call(
                    super::DOMAIN_RULES_TOOL_NAME,
                    args.clone(),
                    Some(&responses),
                    Some(&request_state),
                    true,
                )
                .expect("declined confirmation completes safely");
            assert!(prepared.execution_router.is_none());
            let body = complete_response_body(
                prepared
                    .call
                    .immediate_response
                    .expect("decline returns a complete response"),
            );
            assert_eq!(body["status"], "not_deleted");
            assert_eq!(body["reason"], "confirmation_declined");
        }
        assert_eq!(
            store
                .list_domain_rules(None, None)
                .expect("rule remains")
                .len(),
            1
        );

        let (request_state, _) = issue_overlay_delete_confirmation(
            &service,
            super::DOMAIN_RULES_TOOL_NAME,
            args.clone(),
        );
        let malformed = overlay_confirmation_responses("accept", None);
        let error = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                args.clone(),
                Some(&malformed),
                Some(&request_state),
                true,
            )
            .expect_err("accept without boolean is malformed");
        assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        let correct = overlay_confirmation_responses("accept", Some(true));
        let replay = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                args,
                Some(&correct),
                Some(&request_state),
                true,
            )
            .expect_err("malformed first redemption still consumes the handle");
        assert_eq!(replay.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(
            store
                .list_domain_rules(None, None)
                .expect("malformed response did not delete")
                .len(),
            1
        );
    }

    #[test]
    fn overlay_confirmation_rejects_changed_arguments_missing_state_and_cross_flow_use() {
        let root = tempfile::tempdir().expect("bound confirmation project root");
        let (service, store) = overlay_test_service(root.path());
        let rule_id = store
            .upsert_domain_rule(
                "c",
                "free_fn",
                "bound_free",
                "exact",
                "user",
                "enabled",
                1.0,
                None,
            )
            .expect("bound confirmation rule");
        let args = serde_json::json!({
            "action": "delete",
            "rule_id": rule_id,
            "client_note": "original"
        });
        let responses = overlay_confirmation_responses("accept", Some(true));

        let missing_state = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                args.clone(),
                Some(&responses),
                None,
                true,
            )
            .expect_err("confirmation response without state is rejected");
        assert_eq!(missing_state.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let (request_state, _) = issue_overlay_delete_confirmation(
            &service,
            super::DOMAIN_RULES_TOOL_NAME,
            args.clone(),
        );
        let mut changed = args.clone();
        changed["client_note"] = serde_json::json!("changed");
        let mismatch = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                changed,
                Some(&responses),
                Some(&request_state),
                true,
            )
            .expect_err("any argument change breaks confirmation binding");
        assert_eq!(mismatch.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        let consumed = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                args.clone(),
                Some(&responses),
                Some(&request_state),
                true,
            )
            .expect_err("mismatched redemption consumes the state");
        assert_eq!(consumed.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let (cross_flow_state, _) =
            issue_overlay_delete_confirmation(&service, super::DOMAIN_RULES_TOOL_NAME, args);
        let cross_flow = service
            .prepare_adapter_tool_call(
                super::PROJECT_TOOL_NAME,
                serde_json::json!({"action": "status"}),
                Some(&responses),
                Some(&cross_flow_state),
                true,
            )
            .expect_err("confirmation state cannot authorize another tool");
        assert_eq!(cross_flow.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        assert_eq!(
            store
                .list_domain_rules(None, None)
                .expect("binding failures preserve the rule")
                .len(),
            1
        );
    }

    #[test]
    fn overlay_confirmation_state_expires_is_bounded_and_redeems_atomically() {
        let root = tempfile::tempdir().expect("bounded confirmation project root");
        let (service, _) = overlay_test_service(root.path());
        let args = serde_json::json!({"action": "delete", "rule_id": "rule:ttl"});
        let (expired_state, _) = issue_overlay_delete_confirmation(
            &service,
            super::DOMAIN_RULES_TOOL_NAME,
            args.clone(),
        );
        {
            let mut pending = service
                .overlay_delete_confirmations
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            pending
                .get_mut(&expired_state)
                .expect("pending state")
                .created_at = std::time::Instant::now()
                - std::time::Duration::from_secs(super::OVERLAY_DELETE_CONFIRMATION_TTL_SECS + 1);
        }
        let responses = overlay_confirmation_responses("accept", Some(true));
        let expired = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                args,
                Some(&responses),
                Some(&expired_state),
                true,
            )
            .expect_err("expired state is rejected");
        assert_eq!(expired.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        for index in 0..super::MAX_PENDING_OVERLAY_DELETE_CONFIRMATIONS {
            issue_overlay_delete_confirmation(
                &service,
                super::DOMAIN_RULES_TOOL_NAME,
                serde_json::json!({
                    "action": "delete",
                    "rule_id": format!("rule:{index}")
                }),
            );
        }
        let full = service
            .prepare_adapter_tool_call(
                super::DOMAIN_RULES_TOOL_NAME,
                serde_json::json!({"action": "delete", "rule_id": "rule:overflow"}),
                None,
                None,
                true,
            )
            .expect_err("the pending confirmation map has a hard bound");
        assert_eq!(full.code, rmcp::model::ErrorCode::INTERNAL_ERROR);

        let concurrent_root = tempfile::tempdir().expect("concurrent confirmation root");
        let (service, _) = overlay_test_service(concurrent_root.path());
        let service = std::sync::Arc::new(service);
        let args = serde_json::json!({"action": "delete", "rule_id": "rule:race"});
        let (request_state, _) = issue_overlay_delete_confirmation(
            &service,
            super::DOMAIN_RULES_TOOL_NAME,
            args.clone(),
        );
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut joins = Vec::new();
        for _ in 0..2 {
            let service = std::sync::Arc::clone(&service);
            let barrier = std::sync::Arc::clone(&barrier);
            let args = args.clone();
            let request_state = request_state.clone();
            joins.push(std::thread::spawn(move || {
                let responses = overlay_confirmation_responses("accept", Some(true));
                barrier.wait();
                service
                    .prepare_adapter_tool_call(
                        super::DOMAIN_RULES_TOOL_NAME,
                        args,
                        Some(&responses),
                        Some(&request_state),
                        true,
                    )
                    .is_ok()
            }));
        }
        barrier.wait();
        let accepted: usize = joins
            .into_iter()
            .map(|join| usize::from(join.join().expect("redemption thread")))
            .sum();
        assert_eq!(accepted, 1, "only one concurrent redemption may win");
    }

    #[test]
    fn accepted_fp_delete_reuses_existing_cleanup_path() {
        let root = tempfile::tempdir().expect("FP confirmation project root");
        let (service, store) = overlay_test_service(root.path());
        let field_file = atlas_engine::FileId::generate("src/field.c");
        let target_file = atlas_engine::FileId::generate("src/target.c");
        let range = atlas_engine::TextRange {
            start_byte: 0,
            end_byte: 10,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 11,
        };
        let field_id =
            atlas_engine::SymbolId::generate(&field_file, "c", "Curl_handler.do_it", "field", None);
        let target_id =
            atlas_engine::SymbolId::generate(&target_file, "c", "Curl_http", "function", None);
        for (file_id, path, symbol) in [
            (
                field_file,
                "src/field.c",
                atlas_engine::SymbolDef {
                    id: field_id,
                    kind: atlas_engine::SymbolKind::Field,
                    name: "do_it".into(),
                    qualified_name: "Curl_handler.do_it".into(),
                    symbol_path: vec!["do_it".into()],
                    file_id: field_file,
                    language: atlas_engine::Language::C,
                    range,
                    name_range: range,
                    signature: None,
                    visibility: None,
                    exported: false,
                    static_: false,
                    async_: false,
                    container: None,
                    scope_id: None,
                    package_name: None,
                    namespace_path: vec![],
                    layer: "structural".into(),
                },
            ),
            (
                target_file,
                "src/target.c",
                atlas_engine::SymbolDef {
                    id: target_id,
                    kind: atlas_engine::SymbolKind::Function,
                    name: "Curl_http".into(),
                    qualified_name: "Curl_http".into(),
                    symbol_path: vec!["Curl_http".into()],
                    file_id: target_file,
                    language: atlas_engine::Language::C,
                    range,
                    name_range: range,
                    signature: None,
                    visibility: None,
                    exported: false,
                    static_: false,
                    async_: false,
                    container: None,
                    scope_id: None,
                    package_name: None,
                    namespace_path: vec![],
                    layer: "structural".into(),
                },
            ),
        ] {
            store
                .insert_file_facts(&atlas_engine::FileFacts {
                    file: atlas_engine::FileInfo {
                        file_id,
                        path: path.into(),
                        language: atlas_engine::Language::C,
                        content_hash: format!("hash-{path}"),
                        status: atlas_engine::ParseStatus::Success,
                    },
                    symbols: vec![symbol],
                    ..Default::default()
                })
                .expect("FP confirmation symbol facts");
        }
        let hex = blake3::hash(field_id.as_bytes()).to_hex();
        let annotation_id = format!("fpa:{}:do_it", &hex[..16]);
        store
            .upsert_fp_annotation(&atlas_engine::FpAnnotation {
                annotation_id: annotation_id.clone(),
                source_symbol: field_id,
                field_name: "do_it".into(),
                target_symbol: target_id,
                confidence: atlas_engine::Confidence::new(1.0),
            })
            .expect("seed FP annotation");
        assert_eq!(
            atlas_engine::materialize_annotations(&store).expect("materialize annotation"),
            1
        );
        assert!(!store.find_edges_by_source(&field_id).unwrap().is_empty());

        let args = serde_json::json!({
            "action": "delete",
            "annotation_id": annotation_id
        });
        let (request_state, _) = issue_overlay_delete_confirmation(
            &service,
            super::FP_DISPATCHES_TOOL_NAME,
            args.clone(),
        );
        let responses = overlay_confirmation_responses("accept", Some(true));
        let prepared = service
            .prepare_adapter_tool_call(
                super::FP_DISPATCHES_TOOL_NAME,
                args,
                Some(&responses),
                Some(&request_state),
                true,
            )
            .expect("accepted FP confirmation prepares core delete");
        let router = prepared
            .execution_router
            .expect("FP delete is pinned to the original project");
        let (deleted, delete_error) =
            call_tool_json(&router, super::FP_DISPATCHES_TOOL_NAME, &prepared.call.args);
        assert!(!delete_error, "{deleted:?}");
        assert_eq!(deleted["status"], "deleted");
        assert!(store.get_all_fp_annotations().unwrap().is_empty());
        assert!(store.find_edges_by_source(&field_id).unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn overlay_delete_confirmation_round_trips_over_rmcp_wire() {
        let root = tempfile::tempdir().expect("wire overlay project root");
        let (service, store) = overlay_test_service(root.path());
        let rule_id = store
            .upsert_domain_rule(
                "c",
                "free_fn",
                "wire_confirmed_free",
                "exact",
                "user",
                "enabled",
                1.0,
                None,
            )
            .expect("wire confirmation rule");
        let arguments = serde_json::json!({
            "action": "delete",
            "rule_id": rule_id,
            "client_note": "wire-preserved"
        });
        let argument_object = arguments
            .as_object()
            .expect("wire arguments are an object")
            .clone();

        let (server_transport, client_transport) = tokio::io::duplex(16_384);
        let server = tokio::spawn(async move {
            service.serve(server_transport).await?.waiting().await?;
            anyhow::Ok(())
        });
        let client = TaskInputClient
            .serve(client_transport)
            .await
            .expect("form-capable wire client connects");

        let first = client
            .peer()
            .call_tool_once(
                rmcp::model::CallToolRequestParams::new(super::DOMAIN_RULES_TOOL_NAME)
                    .with_arguments(argument_object.clone()),
            )
            .await
            .expect("initial wire delete returns confirmation");
        let rmcp::model::CallToolResponse::InputRequired(input_required) = first else {
            panic!("wire delete must return input_required");
        };
        let request_state = input_required
            .request_state
            .expect("wire response carries requestState");
        uuid::Uuid::parse_str(&request_state).expect("wire state remains opaque UUID");
        assert_eq!(
            input_required
                .input_requests
                .as_ref()
                .expect("wire input requests")
                .len(),
            1
        );
        assert_eq!(
            store
                .list_domain_rules(None, None)
                .expect("rule remains before wire acceptance")
                .len(),
            1
        );

        let accepted = client
            .peer()
            .call_tool_once(
                rmcp::model::CallToolRequestParams::new(super::DOMAIN_RULES_TOOL_NAME)
                    .with_arguments(argument_object)
                    .with_input_responses(overlay_confirmation_responses("accept", Some(true)))
                    .with_request_state(request_state.clone()),
            )
            .await
            .expect("wire confirmation acceptance completes");
        let rmcp::model::CallToolResponse::Complete(result) = accepted else {
            panic!("accepted wire delete must complete");
        };
        let wire = serde_json::to_value(result).expect("wire delete result serializes");
        let body: serde_json::Value = serde_json::from_str(
            wire["content"][0]["text"]
                .as_str()
                .expect("wire delete returns text"),
        )
        .expect("wire delete returns JSON");
        assert_eq!(body["deleted"], arguments["rule_id"]);
        assert!(
            store
                .list_domain_rules(None, None)
                .expect("wire accepted rule list")
                .is_empty()
        );

        let replay = client
            .peer()
            .call_tool_once(
                rmcp::model::CallToolRequestParams::new(super::DOMAIN_RULES_TOOL_NAME)
                    .with_arguments(
                        arguments
                            .as_object()
                            .expect("replay arguments object")
                            .clone(),
                    )
                    .with_input_responses(overlay_confirmation_responses("accept", Some(true)))
                    .with_request_state(request_state),
            )
            .await
            .expect_err("wire replay must be rejected");
        assert!(
            replay
                .to_string()
                .contains("unknown, expired, or already used"),
            "unexpected replay error: {replay}"
        );

        client
            .cancel()
            .await
            .expect("wire confirmation client shutdown");
        server
            .await
            .expect("wire confirmation server join")
            .expect("wire confirmation server");
    }

    #[test]
    fn project_path_mrtr_requires_open_missing_path_and_form_support() {
        let modern_missing = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::json!({"action": "open"}),
            None,
            None,
            true,
        )
        .expect("modern form client should receive project path input");
        assert!(!modern_missing.allow_candidate_mrtr);
        assert!(matches!(
            modern_missing.immediate_response,
            Some(rmcp::model::CallToolResponse::InputRequired(_))
        ));

        let empty_path = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::json!({"action": "open", "project_path": ""}),
            None,
            None,
            true,
        )
        .expect("empty project path should be elicited");
        assert!(empty_path.immediate_response.is_some());

        for (args, supported) in [
            (serde_json::json!({"action": "open"}), false),
            (serde_json::json!({"action": "status"}), true),
            (
                serde_json::json!({"action": "open", "project_path": "/tmp"}),
                true,
            ),
            (
                serde_json::json!({"action": "open", "project_path": 7}),
                true,
            ),
            (
                serde_json::json!({"action": "open", "project_path": null}),
                true,
            ),
            (
                serde_json::json!({"action": "open", "project_path": "  "}),
                true,
            ),
        ] {
            let prepared = super::AtlasMcpService::prepare_tool_call(
                super::PROJECT_TOOL_NAME,
                args,
                None,
                None,
                supported,
            )
            .expect("non-triggering project calls keep the existing path");
            assert!(!prepared.allow_candidate_mrtr);
            assert!(prepared.immediate_response.is_none());
        }
    }

    #[test]
    fn project_path_input_required_has_a_bounded_stateless_string_form() {
        let response = super::AtlasMcpService::project_path_input_required()
            .expect("project path elicitation should build");
        let rmcp::model::CallToolResponse::InputRequired(input_required) = response else {
            panic!("project path preflight must return input_required");
        };
        let wire = serde_json::to_value(input_required).expect("input_required serializes");

        assert_eq!(wire["resultType"], "input_required");
        assert!(wire.get("requestState").is_none());
        let request = &wire["inputRequests"][super::PROJECT_PATH_INPUT_ID];
        assert_eq!(request["method"], "elicitation/create");
        assert_eq!(request["params"]["mode"], "form");
        let schema = &request["params"]["requestedSchema"];
        assert_eq!(schema["required"], serde_json::json!(["project_path"]));
        assert_eq!(schema["properties"]["project_path"]["type"], "string");
        assert_eq!(schema["properties"]["project_path"]["minLength"], 1);
        assert_eq!(
            schema["properties"]["project_path"]["maxLength"],
            super::tools::MAX_FILE_PATH_LENGTH
        );
        assert!(
            schema["properties"]["project_path"]["description"]
                .as_str()
                .is_some_and(|description| description.contains("Absolute path"))
        );
    }

    #[test]
    fn accepted_project_path_preserves_arguments_and_reuses_the_core_open_handler() {
        let project = tempfile::tempdir().expect("temporary project directory");
        let project_path = project.path().to_string_lossy().into_owned();
        let responses = project_path_responses("accept", Some(&project_path));
        let prepared = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::json!({"action": "open", "client_note": "preserved"}),
            Some(&responses),
            None,
            true,
        )
        .expect("accepted project path should prepare the original call");

        assert_eq!(prepared.args["action"], "open");
        assert_eq!(prepared.args["project_path"], project_path);
        assert_eq!(prepared.args["client_note"], "preserved");
        assert!(prepared.immediate_response.is_none());

        let service = super::AtlasMcpService::new_unopened();
        let result = service.router.call_tool(
            &super::tools::ToolCallContext::empty(),
            super::PROJECT_TOOL_NAME,
            &prepared.args,
        );
        assert_eq!(result.is_error, Some(false));
        let text = match result.content.first().expect("project result content") {
            super::protocol::ContentBlock::Text { text } => text,
        };
        let body: serde_json::Value = serde_json::from_str(text).expect("project result JSON");
        assert_eq!(
            body["active_project"],
            project.path().canonicalize().unwrap().display().to_string()
        );
        assert!(project.path().join(".atlas/atlas.db").is_file());
    }

    #[test]
    fn declined_or_cancelled_project_path_falls_back_without_opening_or_looping() {
        let baseline_service = super::AtlasMcpService::new_unopened();
        let baseline = baseline_service.router.call_tool(
            &super::tools::ToolCallContext::empty(),
            super::PROJECT_TOOL_NAME,
            &serde_json::json!({"action": "open"}),
        );
        let baseline_wire = serde_json::to_value(&baseline).expect("baseline result serializes");

        for action in ["decline", "cancel"] {
            let responses = project_path_responses(action, None);
            let prepared = super::AtlasMcpService::prepare_tool_call(
                super::PROJECT_TOOL_NAME,
                serde_json::json!({"action": "open"}),
                Some(&responses),
                None,
                true,
            )
            .expect("decline and cancel should return the existing missing-path result");
            assert!(prepared.immediate_response.is_none());
            assert!(prepared.args.get("project_path").is_none());

            let service = super::AtlasMcpService::new_unopened();
            let result = service.router.call_tool(
                &super::tools::ToolCallContext::empty(),
                super::PROJECT_TOOL_NAME,
                &prepared.args,
            );
            assert_eq!(result.is_error, Some(true));
            assert_eq!(
                serde_json::to_value(&result).expect("fallback result serializes"),
                baseline_wire,
                "{action} must preserve the exact existing core result"
            );
            let text = match result.content.first().expect("project error content") {
                super::protocol::ContentBlock::Text { text } => text,
            };
            assert!(text.contains("Missing required parameter: project_path"));
        }
    }

    #[test]
    fn malformed_project_path_retry_and_request_state_are_rejected() {
        for responses in [
            project_path_responses("accept", None),
            project_path_responses("accept", Some("")),
            project_path_responses("unexpected", None),
        ] {
            let error = super::AtlasMcpService::prepare_tool_call(
                super::PROJECT_TOOL_NAME,
                serde_json::json!({"action": "open"}),
                Some(&responses),
                None,
                true,
            )
            .expect_err("malformed project path retry must fail");
            assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);
        }

        let mut non_string = rmcp::model::InputResponses::new();
        non_string.insert(
            super::PROJECT_PATH_INPUT_ID.into(),
            serde_json::json!({
                "action": "accept",
                "content": {super::PROJECT_PATH_FIELD: 7}
            }),
        );
        let non_string_error = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::json!({"action": "open"}),
            Some(&non_string),
            None,
            true,
        )
        .expect_err("accepted project path must be a string");
        assert_eq!(
            non_string_error.code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );

        let mut extra = project_path_responses("cancel", None);
        extra.insert("extra".into(), serde_json::json!({"action": "cancel"}));
        let extra_error = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::json!({"action": "open"}),
            Some(&extra),
            None,
            true,
        )
        .expect_err("project path retry must contain exactly one response");
        assert_eq!(extra_error.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let missing_arguments = project_path_responses("accept", Some("/tmp"));
        let error = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::Value::Null,
            Some(&missing_arguments),
            None,
            true,
        )
        .expect_err("retry must preserve the original argument object");
        assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let wrong_action = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::json!({"action": "status"}),
            Some(&project_path_responses("accept", Some("/tmp"))),
            None,
            true,
        )
        .expect_err("retry must preserve action=open");
        assert_eq!(wrong_action.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let mut unknown = rmcp::model::InputResponses::new();
        unknown.insert(
            super::EXPLORE_CANDIDATE_INPUT_ID.into(),
            serde_json::json!({"action": "cancel"}),
        );
        let unknown_error = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::json!({"action": "open"}),
            Some(&unknown),
            None,
            true,
        )
        .expect_err("project path flow must not consume explore responses");
        assert_eq!(unknown_error.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let state_error = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::json!({"action": "open"}),
            None,
            Some("untrusted-state"),
            true,
        )
        .expect_err("project path flow does not use requestState");
        assert_eq!(state_error.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let unsupported_error = super::AtlasMcpService::prepare_tool_call(
            super::PROJECT_TOOL_NAME,
            serde_json::json!({"action": "open"}),
            Some(&project_path_responses("decline", None)),
            None,
            false,
        )
        .expect_err("legacy and non-interactive clients cannot retry project path MRTR");
        assert_eq!(
            unsupported_error.code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );
    }

    #[test]
    fn ambiguous_explore_builds_bounded_stateless_elicitation() {
        let result = ambiguous_explore_result();
        let input_required = super::AtlasMcpService::explore_candidate_input_required(&result)
            .expect("ambiguous explore should require candidate input");
        assert!(input_required.request_state.is_none());

        let wire = serde_json::to_value(input_required).expect("input_required serializes");
        assert_eq!(wire["resultType"], "input_required");
        assert!(wire.get("requestState").is_none());
        let request = &wire["inputRequests"][super::EXPLORE_CANDIDATE_INPUT_ID];
        let task_request = serde_json::to_value(
            super::AtlasMcpService::explore_candidate_input_request(&result)
                .expect("task candidate input should reuse the direct MRTR request"),
        )
        .expect("task candidate request serializes");
        assert_eq!(&task_request, request);
        assert_eq!(request["method"], "elicitation/create");
        assert_eq!(request["params"]["mode"], "form");
        let choices = request["params"]["requestedSchema"]["properties"]
            [super::EXPLORE_CANDIDATE_FIELD]["oneOf"]
            .as_array()
            .expect("titled enum should serialize as oneOf");
        assert_eq!(choices.len(), 2);
        for (choice, expected_path) in choices.iter().zip(["src/a.rs", "src/b.rs"]) {
            let selector: serde_json::Value = serde_json::from_str(
                choice["const"]
                    .as_str()
                    .expect("candidate value should be a JSON string"),
            )
            .expect("candidate value should decode as JSON");
            assert_eq!(selector["file_path"], expected_path);
            assert!(choice["title"].as_str().is_some_and(|title| {
                title.contains("shared_func") && title.contains(expected_path)
            }));
        }
    }

    #[test]
    fn candidate_mrtr_only_replaces_true_ambiguous_explore_results() {
        let ambiguous =
            super::AtlasMcpService::to_rmcp_response("explore", ambiguous_explore_result(), true);
        assert!(matches!(
            ambiguous,
            rmcp::model::CallToolResponse::InputRequired(_)
        ));

        let legacy_or_noninteractive =
            super::AtlasMcpService::to_rmcp_response("explore", ambiguous_explore_result(), false);
        assert!(matches!(
            legacy_or_noninteractive,
            rmcp::model::CallToolResponse::Complete(_)
        ));

        let deterministic = super::protocol::CallToolResult {
            content: vec![super::protocol::ContentBlock::text(
                serde_json::json!({"symbol": "unique", "resolution": {}}).to_string(),
            )],
            is_error: Some(false),
        };
        assert!(matches!(
            super::AtlasMcpService::to_rmcp_response("explore", deterministic, true),
            rmcp::model::CallToolResponse::Complete(_)
        ));
    }

    #[test]
    fn accepted_candidate_rewrites_only_symbol_and_disables_another_round() {
        let selector = serde_json::json!({
            "qualified_name": "shared_func",
            "file_path": "src/b.rs",
            "line": 20,
            "kind": "function",
            "language": "rust"
        });
        let responses = explore_selection_responses("accept", Some(&selector));
        let prepared = super::AtlasMcpService::prepare_tool_call(
            "explore",
            serde_json::json!({
                "symbol": "shared_func",
                "source_mode": "none",
                "relation_limit": 7
            }),
            Some(&responses),
            None,
            true,
        )
        .expect("valid accepted candidate should prepare a retry");

        assert_eq!(prepared.args["symbol"], selector);
        assert_eq!(prepared.args["source_mode"], "none");
        assert_eq!(prepared.args["relation_limit"], 7);
        assert!(!prepared.allow_candidate_mrtr);
    }

    #[test]
    fn stateless_candidate_retry_treats_selection_as_an_ordinary_selector_input() {
        let selector = serde_json::json!({
            "qualified_name": "different_symbol",
            "file_path": "src/other.rs",
            "line": 42
        });
        let responses = explore_selection_responses("accept", Some(&selector));
        let prepared = super::AtlasMcpService::prepare_tool_call(
            super::EXPLORE_TOOL_NAME,
            serde_json::json!({"symbol": "original_ambiguous_name", "scope": "src"}),
            Some(&responses),
            None,
            true,
        )
        .expect("stateless selection is validated like an ordinary selector input");

        assert_eq!(prepared.args["symbol"], selector);
        assert_eq!(prepared.args["scope"], "src");
        assert!(!prepared.allow_candidate_mrtr);
    }

    #[test]
    fn declined_or_cancelled_candidate_falls_back_without_looping() {
        let args = serde_json::json!({"symbol": "shared_func", "source_mode": "full"});
        for action in ["decline", "cancel"] {
            let responses = explore_selection_responses(action, None);
            let prepared = super::AtlasMcpService::prepare_tool_call(
                "explore",
                args.clone(),
                Some(&responses),
                None,
                true,
            )
            .expect("decline and cancel should return the original complete result");
            assert_eq!(prepared.args, args);
            assert!(!prepared.allow_candidate_mrtr);
        }
    }

    #[test]
    fn malformed_candidate_retry_and_request_state_are_rejected() {
        let missing_selection = explore_selection_responses("accept", None);
        let error = super::AtlasMcpService::prepare_tool_call(
            "explore",
            serde_json::json!({"symbol": "shared_func"}),
            Some(&missing_selection),
            None,
            true,
        )
        .expect_err("accepted response without selection must fail");
        assert_eq!(error.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let selector = serde_json::json!({"qualified_name": "shared_func"});
        let missing_arguments = explore_selection_responses("accept", Some(&selector));
        let missing_arguments_error = super::AtlasMcpService::prepare_tool_call(
            "explore",
            serde_json::Value::Null,
            Some(&missing_arguments),
            None,
            true,
        )
        .expect_err("stateless retry must preserve the original arguments");
        assert_eq!(
            missing_arguments_error.code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );

        let non_object_selection = serde_json::json!("shared_func");
        let non_object_responses =
            explore_selection_responses("accept", Some(&non_object_selection));
        let non_object_error = super::AtlasMcpService::prepare_tool_call(
            "explore",
            serde_json::json!({"symbol": "shared_func"}),
            Some(&non_object_responses),
            None,
            true,
        )
        .expect_err("selection must decode to a SymbolSelector object");
        assert_eq!(
            non_object_error.code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );

        let mut unknown_responses = rmcp::model::InputResponses::new();
        unknown_responses.insert(
            "unexpected".into(),
            serde_json::json!({"action": "decline"}),
        );
        let unknown_error = super::AtlasMcpService::prepare_tool_call(
            "explore",
            serde_json::json!({"symbol": "shared_func"}),
            Some(&unknown_responses),
            None,
            true,
        )
        .expect_err("unknown input response must not be guessed");
        assert_eq!(unknown_error.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let state_error = super::AtlasMcpService::prepare_tool_call(
            "explore",
            serde_json::json!({"symbol": "shared_func"}),
            None,
            Some("untrusted-state"),
            true,
        )
        .expect_err("requestState is not used by this flow");
        assert_eq!(state_error.code, rmcp::model::ErrorCode::INVALID_PARAMS);

        let unsupported_error = super::AtlasMcpService::prepare_tool_call(
            "explore",
            serde_json::json!({"symbol": "shared_func"}),
            Some(&explore_selection_responses("decline", None)),
            None,
            false,
        )
        .expect_err("legacy or non-interactive clients cannot retry MRTR");
        assert_eq!(
            unsupported_error.code,
            rmcp::model::ErrorCode::INVALID_PARAMS
        );
    }

    #[test]
    fn rmcp_tool_conversion_preserves_complete_input_schema_object() {
        let schema = serde_json::json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "$defs": {
                "selector": {
                    "oneOf": [
                        {"type": "string"},
                        {"type": "object", "required": ["qualified_name"]}
                    ]
                }
            },
            "properties": {"symbol": {"$ref": "#/$defs/selector"}},
            "required": ["symbol"],
            "if": {"required": ["symbol"]},
            "then": {"additionalProperties": false},
            "else": {"maxProperties": 0},
            "oneOf": [{"required": ["symbol"]}],
            "additionalProperties": false,
            "x-atlas-future-keyword": {"preserve": true}
        });
        let schema_object = schema.as_object().expect("schema is an object").clone();
        let tool = super::protocol::Tool {
            name: "schema_fidelity".into(),
            description: "schema fidelity regression".into(),
            input_schema: super::protocol::ToolInputSchema::from_object(schema_object.clone()),
        };

        let converted = super::AtlasMcpService::to_rmcp_tool(tool);
        assert_eq!(converted.input_schema.as_ref(), &schema_object);
        assert_eq!(
            serde_json::to_value(converted).expect("rmcp tool serializes")["inputSchema"],
            schema
        );
    }

    #[test]
    fn current_tool_catalog_schemas_match_rmcp_models_in_order() {
        let expected_names = [
            "project",
            "search",
            "symbol",
            "calls",
            "explore",
            "path",
            "impact",
            "file_dependencies",
            "trace",
            "lifecycle",
            "branch_diff",
            "domain_rules",
            "fp_dispatches",
            "tasks",
            "resume_query",
        ];
        let tools = super::make_all_tools();
        assert_eq!(tools.len(), expected_names.len());

        for (tool, expected_name) in tools.into_iter().zip(expected_names) {
            assert_eq!(tool.name, expected_name);
            let expected_schema = tool.input_schema.as_object().clone();
            let expected_description = tool.description.clone();
            let converted = super::AtlasMcpService::to_rmcp_tool(tool);

            assert_eq!(converted.name.as_ref(), expected_name);
            assert_eq!(
                converted.description.as_deref(),
                Some(expected_description.as_str())
            );
            assert_eq!(converted.input_schema.as_ref(), &expected_schema);
        }
    }

    #[test]
    fn action_dependent_catalog_constraints_round_trip_to_rmcp_wire() {
        for name in ["domain_rules", "fp_dispatches"] {
            let tool = super::make_all_tools()
                .into_iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name} tool must exist"));
            let expected = tool
                .input_schema
                .get("allOf")
                .cloned()
                .unwrap_or_else(|| panic!("{name} must define action constraints"));

            let converted = super::AtlasMcpService::to_rmcp_tool(tool);
            let wire = serde_json::to_value(converted).expect("rmcp tool serializes");
            assert_eq!(
                wire["inputSchema"]["allOf"], expected,
                "{name} action constraints changed across the rmcp boundary"
            );
        }
    }

    #[test]
    fn tool_catalog_cache_metadata_is_version_gated() {
        let service = super::AtlasMcpService::new_unopened();
        let tools = service
            .router
            .list_tools()
            .tools
            .into_iter()
            .map(super::AtlasMcpService::to_rmcp_tool)
            .collect::<Vec<_>>();

        let unknown = super::AtlasMcpService::list_tools_result(tools.clone(), None);
        let legacy = super::AtlasMcpService::list_tools_result(
            tools.clone(),
            Some(&rmcp::model::ProtocolVersion::V_2025_11_25),
        );
        let modern = super::AtlasMcpService::list_tools_result(
            tools,
            Some(&rmcp::model::ProtocolVersion::V_2026_07_28),
        );

        assert_eq!(unknown.ttl_ms, None);
        assert_eq!(unknown.cache_scope, None);
        assert_eq!(legacy.ttl_ms, None);
        assert_eq!(legacy.cache_scope, None);
        assert_eq!(modern.ttl_ms, Some(super::TOOL_CATALOG_TTL_MS));
        assert_eq!(modern.cache_scope, Some(rmcp::model::CacheScope::Public));
    }

    #[test]
    fn tool_catalog_wire_shape_preserves_catalog_and_legacy_result_type() {
        let service = super::AtlasMcpService::new_unopened();
        let tools = service
            .router
            .list_tools()
            .tools
            .into_iter()
            .map(super::AtlasMcpService::to_rmcp_tool)
            .collect::<Vec<_>>();
        assert_eq!(tools.len(), 15);

        let no_version = super::AtlasMcpService::list_tools_result(tools.clone(), None);
        let legacy = super::AtlasMcpService::list_tools_result(
            tools.clone(),
            Some(&rmcp::model::ProtocolVersion::V_2025_11_25),
        );
        let modern = super::AtlasMcpService::list_tools_result(
            tools,
            Some(&rmcp::model::ProtocolVersion::V_2026_07_28),
        );
        assert_eq!(legacy.tools, modern.tools);

        let modern_wire = serde_json::to_value(&modern).expect("modern list_tools serializes");
        assert_eq!(modern_wire["ttlMs"], super::TOOL_CATALOG_TTL_MS);
        assert_eq!(modern_wire["cacheScope"], "public");
        assert_eq!(modern_wire["resultType"], "complete");

        for result in [no_version, legacy] {
            let mut legacy_wire_result = rmcp::model::ServerResult::ListToolsResult(result);
            legacy_wire_result.strip_result_type_for_legacy_peer();
            let legacy_wire =
                serde_json::to_value(legacy_wire_result).expect("legacy list_tools serializes");
            assert!(legacy_wire.get("ttlMs").is_none());
            assert!(legacy_wire.get("cacheScope").is_none());
            assert!(legacy_wire.get("resultType").is_none());
        }
    }

    #[test]
    fn already_negotiated_newer_date_version_uses_adapter_forward_policy() {
        // rmcp 3.0.1 negotiates only its supported versions; this locks the adapter policy
        // once a future newer date version is supported and already negotiated.
        let newer: rmcp::model::ProtocolVersion =
            serde_json::from_value(serde_json::json!("2027-01-01"))
                .expect("date-version protocol parses");
        let result = super::AtlasMcpService::list_tools_result(Vec::new(), Some(&newer));
        assert_eq!(result.ttl_ms, Some(super::TOOL_CATALOG_TTL_MS));
        assert_eq!(result.cache_scope, Some(rmcp::model::CacheScope::Public));
    }
}
