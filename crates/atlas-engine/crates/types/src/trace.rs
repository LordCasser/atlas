//! Atlas trace types — location-driven variable tracking.
//!
//! ## Core concept
//!
//! Trace queries let users ask "where does this value come from?" by clicking
//! a source position and walking backward through dataflow edges (use-def chain).
//! Unlike forward propagation from pre-defined sources, trace starts from a
//! user-chosen point and slices backward.
//!
//! ## Key types
//!
//! - [`TracePoint`] — the resolved state at a source position (symbol, data
//!   node, binding, scope, incoming/outgoing flows)
//! - [`TracePath`] — a backward slice from a point to its origins
//! - [`TracePathStep`] — a single step in a trace path (source→target data
//!   node with edge kind)
//!
//! ## Relationship with other types
//!
//! - [`TracePoint::reference`] → [`super::structs::ReferenceUse`]
//! - [`TracePoint::data_node`] → [`super::dataflow::DataNode`]
//! - [`TracePoint::binding`] → [`super::bindings::BindingDef`]
//! - [`TracePathStep::edge_kind`] → [`super::enums::DataFlowKind`]

use serde::{Deserialize, Serialize};

use super::bindings::{BindingDef, BindingUse};
use super::capability::LanguageCapabilityProfile;
use super::dataflow::DataNode;
use super::enums::DataFlowKind;
use super::ids::{DataNodeId, FileId};
use super::structs::{Callsite, DiagnosticLevel, ReferenceUse, ScopeDef, SymbolDef, TextRange};
use super::structs::precision::PrecisionTier;

// ---------------------------------------------------------------------------
// TraceDiagnostic — a structured hint/warning/error for trace results
// ---------------------------------------------------------------------------

/// A diagnostic message attached to a trace result, indicating data quality
/// or analysis limitations.  Not an error — the result is still delivered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDiagnostic {
    pub level: DiagnosticLevel,
    pub message: String,
    /// Optional machine-readable code (e.g. "no_data_node", "unsupported_language").
    pub code: Option<String>,
    /// Optional structured detail payload (e.g. boundary marker JSON).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl TraceDiagnostic {
    pub fn info(message: &str) -> Self {
        Self {
            level: DiagnosticLevel::Info,
            message: message.to_string(),
            code: None,
            detail: None,
        }
    }
    pub fn warning(message: &str) -> Self {
        Self {
            level: DiagnosticLevel::Warning,
            message: message.to_string(),
            code: None,
            detail: None,
        }
    }
    pub fn error(message: &str) -> Self {
        Self {
            level: DiagnosticLevel::Error,
            message: message.to_string(),
            code: None,
            detail: None,
        }
    }
    pub fn with_code(mut self, code: &str) -> Self {
        self.code = Some(code.to_string());
        self
    }

    /// Attach a structured detail payload (e.g. a serialized BoundaryMarker).
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

// ---------------------------------------------------------------------------
// Evidence — human-readable context for trace response consumers
// ---------------------------------------------------------------------------

/// Human-readable evidence attached to a trace response.
///
/// Provides file path, code snippet, and symbol name so that agent/AI consumers
/// can present contextual information without additional database queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    /// The file path this evidence points to.
    pub file_path: String,
    /// A small code snippet (read from the file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// The primary symbol name for this evidence point.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub symbol_name: Option<String>,
}

// ---------------------------------------------------------------------------
// TracePoint — resolved state at a source position
// ---------------------------------------------------------------------------

/// The full resolved state at a user-chosen source position.
///
/// A locator takes `(file_id, line, column)` and returns a `TracePoint`
/// containing everything that is known about that position: the enclosing
/// reference, the symbol it resolves to, the data node that "contains" the
/// value, incident dataflow edges, the lexical binding, and the enclosing
/// scope.  Not every field is always present — the locator populates what it
/// can find.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracePoint {
    /// The reference at this source position (if the position falls inside a
    /// reference's byte range).
    pub reference: Option<ReferenceUse>,
    /// The symbol that this reference resolves to (the definition).
    pub resolved_symbol: Option<SymbolDef>,
    /// The data node whose byte range contains this position.  Data nodes
    /// represent a point where data exists (parameter, local, field access,
    /// call argument, etc.).
    pub data_node: Option<DataNode>,
    /// Data nodes that flow **into** this node (incoming dataflow edges).
    pub incoming: Vec<TraceDataNodeRef>,
    /// Data nodes that this node flows **into** (outgoing dataflow edges).
    pub outgoing: Vec<TraceDataNodeRef>,
    /// The lexical binding at this position (variable/parameter declaration).
    pub binding: Option<BindingDef>,
    /// The binding use at this position.
    pub binding_use: Option<BindingUse>,
    /// The innermost scope enclosing this position.
    pub scope: Option<ScopeDef>,
    /// The callsite if this position is inside a call expression.
    pub callsite: Option<Callsite>,
    /// The file this point resides in.
    pub file_id: FileId,
    /// The query position used to locate this point (1-based line).
    pub line: u32,
    /// The query position used to locate this point (1-based column).
    pub column: u32,
    /// The analysis capability for the language of this point (resolved from
    /// the symbol's language, if a symbol was found).
    #[serde(default)]
    pub capability: Option<LanguageCapabilityProfile>,
    /// If true, the trace produced a partial result (e.g., dataflow sliced
    /// some edges but couldn't reach the origin).
    #[serde(default)]
    pub partial_result: bool,
    /// Diagnostics (warnings, notes) produced during location or slicing.
    #[serde(default)]
    pub diagnostics: Vec<TraceDiagnostic>,
}

/// A lightweight reference to a data node that flows into or out of a trace
/// point.  Carries just enough information for display and navigation without
/// embedding the full [`DataNode`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TraceDataNodeRef {
    pub node_id: DataNodeId,
    pub name: String,
    pub kind: String,
    pub access_path: Option<String>,
    pub file_id: FileId,
    pub range: Option<TextRange>,
}

impl TraceDataNodeRef {
    /// Create a `TraceDataNodeRef` from a full [`DataNode`].
    pub fn from_data_node(node: &DataNode) -> Self {
        Self {
            node_id: node.id.clone(),
            name: node.name.clone().unwrap_or_default(),
            kind: node.kind.as_str().to_string(),
            access_path: node.access_path.clone(),
            file_id: node.file_id.clone(),
            range: Some(node.range.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// TracePath — a backward slice from point to origins
// ---------------------------------------------------------------------------

/// A backward dataflow trace from a sink point back to its origins.
///
/// The trace engine walks backward through `Assign`, `Read`, `Write`,
/// `FieldLoad`, `ArgToParam`, and use-def edges to reconstruct how a value
/// reached a particular position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracePath {
    /// Fully resolved source point (the origin of the traced value).
    pub source: TracePoint,
    /// Ordered sequence of steps from source to sink.
    pub steps: Vec<TracePathStep>,
    /// Fully resolved sink point (the user-chosen position).
    pub sink: TracePoint,
    /// How confident the engine is about this path (0.0–1.0).
    pub confidence: f64,
    /// The number of dataflow nodes visited during the trace.
    pub nodes_visited: usize,
    /// How far backward the trace was able to go (number of BFS levels).
    /// Compare against the requested `max_depth` to detect truncation.
    pub max_depth_reached: usize,
    /// Language capability profile. Always present for MCP consumers.
    #[serde(default)]
    pub capability: Option<LanguageCapabilityProfile>,
    /// Best-effort / incomplete result flag.
    #[serde(default)]
    pub partial_result: bool,
    /// Structured diagnostics.
    #[serde(default)]
    pub diagnostics: Vec<TraceDiagnostic>,
    /// Metadata about lazy dataflow loading that occurred during this trace.
    /// None if lazy loading was not triggered (e.g., dataflow already existed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lazy_summary: Option<LazySummary>,
}

/// Summary of lazy dataflow loading triggered by a trace query.
///
/// Agents can use this to understand query performance:
///   - `triggered` + `units_built > 0` → cold start, retry will hit cache
///   - `units_cached > 0` → dataflow was already available
///   - `truncated` → result may be incomplete due to budget limits
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LazySummary {
    /// Whether lazy loading was triggered during this trace.
    pub triggered: bool,
    /// Number of AnalysisUnits whose dataflow was built from scratch.
    pub units_built: usize,
    /// Number of AnalysisUnits whose dataflow was already cached.
    pub units_cached: usize,
    /// Whether any unit hit the internal budget limit (partial result).
    pub truncated: bool,
    /// Wall-clock time spent on lazy dataflow loading (milliseconds).
    pub duration_ms: u64,
    /// Precision tier of the dataflow result (set by lazy dataflow service).
    /// None means no lazy dataflow was triggered or tier is irrelevant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision_tier: Option<PrecisionTier>,
}

/// A single step in a trace path — connects two data nodes via a dataflow edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracePathStep {
    /// Step index (0-based, from source to sink).
    pub index: u32,
    /// The data node at the **from** end of the edge.
    pub from_node_id: DataNodeId,
    /// The data node at the **to** end of the edge.
    pub to_node_id: DataNodeId,
    /// The type of dataflow that connects these nodes.
    pub edge_kind: DataFlowKind,
    /// Human-readable description of this step (e.g. "assign", "field load").
    pub description: String,
    /// The file where this edge occurs.
    pub file_id: FileId,
    /// The source range of the edge (if available).
    pub range: Option<TextRange>,
    /// Human-readable evidence (file path, symbol name) for agent consumption.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Evidence>,
}

// ---------------------------------------------------------------------------
// Document-stable type aliases (for agent contract compatibility)
// ---------------------------------------------------------------------------

/// Document-stable alias for [`TracePath`], used in MCP tool schemas and
/// JSON external contracts.
#[allow(non_camel_case_types)]
pub type VariableTracePath = TracePath;

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

impl TracePathStep {
    /// Create a step from `from_node` to `to_node` with the given edge kind.
    pub fn new(
        index: u32,
        from_node_id: DataNodeId,
        to_node_id: DataNodeId,
        edge_kind: DataFlowKind,
        description: &str,
        file_id: FileId,
        range: Option<TextRange>,
    ) -> Self {
        Self {
            index,
            from_node_id,
            to_node_id,
            edge_kind,
            description: description.to_string(),
            file_id,
            range,
            evidence: None,
        }
    }
}

// ---------------------------------------------------------------------------
// BoundaryMarker — dynamic dispatch / truncation boundary
// ---------------------------------------------------------------------------

/// Marks a boundary where static call-graph tracing cannot continue.
///
/// Unlike silent truncation, a `BoundaryMarker` explicitly tells the consumer
/// WHY the path stopped, and suggests a remediation (e.g. manually exploring
/// the callback target).  This is critical for security analysis to avoid
/// false negatives: a path that "ends" at a callback boundary is NOT the same
/// as a path that truly terminates at a root function.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryMarker {
    /// The type of boundary encountered.
    pub kind: BoundaryKind,
    /// Human-readable message explaining the boundary.
    pub message: String,
    /// Actionable suggestion for the consumer (e.g. "Use explore on 'X'").
    pub suggestion: String,
    /// The symbol to bridge to if manual tracing should continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bridge_target: Option<String>,
}

/// Taxonomy of boundaries that halt static call-graph tracing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum BoundaryKind {
    /// Function pointer call: `void (*ptr)(int)` — target resolved at runtime.
    FunctionPointer { pointer_name: String },
    /// Callback registration: `set_callback(ctx, on_event)` — invoked dynamically.
    CallbackRegistration {
        registrant: String,
        callback: String,
    },
    /// Virtual dispatch: `obj->vtable[idx]()` — resolved by runtime type.
    VirtualDispatch {
        class_name: String,
        method_name: String,
    },
    /// Dynamic method call: `$obj->$method()` (PHP) or `send(method, ...)` (Ruby).
    DynamicMethodCall { receiver_type: String },
    /// Depth limit reached — more callers exist beyond max_depth.
    MaxDepthTruncated {
        depth_reached: usize,
        max_depth: usize,
        has_unexplored: bool,
    },
    /// Root function — no further callers; top of the call chain.
    RootFunction,
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dataflow::DataNode;
    use crate::ids::{DataNodeId, FileId};
    use crate::structs::TextRange;

    #[test]
    fn trace_data_node_ref_from_data_node() {
        let file_id = FileId::generate("test.ts");
        let node_id = DataNodeId::generate(&file_id, None, "local", Some("x"), None, 0);
        let range = TextRange {
            start_byte: 0,
            end_byte: 1,
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 2,
        };
        let node = DataNode::local(node_id, file_id.clone(), None, None, "x", range);
        let ref_ = TraceDataNodeRef::from_data_node(&node);
        assert_eq!(ref_.name, "x");
        assert_eq!(ref_.kind, "local");
        assert_eq!(ref_.node_id, node.id);
    }

    #[test]
    fn trace_path_step_creation() {
        let file_id = FileId::generate("test.ts");
        let from = DataNodeId::generate(&file_id, None, "local", Some("x"), None, 0);
        let to = DataNodeId::generate(&file_id, None, "expr", Some("y"), None, 10);
        let step = TracePathStep::new(
            0,
            from.clone(),
            to.clone(),
            DataFlowKind::Assign,
            "x assigned to y",
            file_id,
            None,
        );
        assert_eq!(step.index, 0);
        assert_eq!(step.from_node_id, from);
        assert_eq!(step.to_node_id, to);
        assert_eq!(step.edge_kind, DataFlowKind::Assign);
        assert_eq!(step.description, "x assigned to y");
    }

    #[test]
    fn trace_point_serialization_roundtrip() {
        // Verify TracePoint can be serialized for MCP transport.
        let file_id = FileId::generate("test.py");
        let tp = TracePoint {
            reference: None,
            resolved_symbol: None,
            data_node: None,
            incoming: vec![],
            outgoing: vec![],
            binding: None,
            binding_use: None,
            scope: None,
            callsite: None,
            file_id,
            line: 10,
            column: 5,
            capability: None,
            partial_result: false,
            diagnostics: vec![],
        };
        let json = serde_json::to_string(&tp).unwrap();
        let _: TracePoint = serde_json::from_str(&json).unwrap();
    }
}
