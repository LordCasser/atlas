//! Lightweight function summaries for query-time cross-procedural bridging.
//!
//! ## Core concept
//!
//! A [`FunctionSummary`] captures intraprocedural reachability from parameters
//! to downstream data nodes (return values, call arguments, field accesses).
//! It is computed on-demand from existing DataNodes + DataFlowEdges in the DB
//! — no schema changes or extraction-time work required.
//!
//! ## Key types
//!
//! - [`FunctionSummary`] — the summary for one function
//! - [`ParameterFlow`] — for each parameter, which downstream nodes it reaches
//! - [`ReturnFlow`] — return statement → upstream source nodes
//! - [`CallArgFlow`] — callsite argument → downstream source data nodes
//!
//! ## Confidence model
//!
//! Each flow carries a `confidence` in [0.0, 1.0]:
//! - 1.0  = direct dataflow edge confirmed (e.g. Assign edge from param)
//! - 0.85 = indirect (BFS across multiple edges)
//! - 0.67 = heuristic (name-based fallback)
//! - 0.0  = unknown
//!
//! ## Relationship with other types
//!
//! - Built from [`super::dataflow::DataNode`] and [`super::dataflow::DataFlowEdge`]
//! - Consumed by trace/caller-path for bounding interprocedural search
//! - `DataNodeId` references are all within the same function
//! - `CallsiteId` references point to `callsites` table entries

use serde::{Deserialize, Serialize};

use super::ids::{CallsiteId, DataNodeId, SymbolId};

/// Intraprocedural summary for a single function.
///
/// Computed query-time by [`crate::analysis::summary::SummaryBuilder`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionSummary {
    /// The function this summary describes.
    pub function_id: SymbolId,
    /// Total count of data nodes in this function.
    pub node_count: usize,
    /// Total count of intraprocedural dataflow edges.
    pub edge_count: usize,
    /// For each parameter node, which downstream effects it has.
    pub param_flows: Vec<ParameterFlow>,
    /// Return statements and their upstream source nodes.
    pub return_flows: Vec<ReturnFlow>,
    /// Call arguments and their upstream source nodes.
    pub call_arg_flows: Vec<CallArgFlow>,
}

/// Downstream reachability from one parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterFlow {
    /// The parameter node this flow originates from.
    pub param_id: DataNodeId,
    /// Parameter index (0-based position in the parameter list).
    pub param_index: usize,
    /// Parameter name (e.g. "req", "options").
    pub param_name: String,
    /// Call-argument nodes this parameter reaches (passed to inner calls).
    pub reaches_call_args: Vec<DataNodeId>,
    /// Return-like nodes this parameter reaches (contributes to return value).
    pub reaches_returns: Vec<DataNodeId>,
    /// Field-load nodes this parameter reaches (member access expressions).
    pub reaches_fields: Vec<DataNodeId>,
    /// Confidence of this flow (see module-level confidence model).
    pub confidence: f64,
    /// Provenance: how this flow was determined (e.g. "intraprocedural_dataflow", "name_heuristic").
    pub provenance: String,
}

/// Return statement and the upstream nodes that feed into it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReturnFlow {
    /// The return-type data node.
    pub return_id: DataNodeId,
    /// Node IDs whose data reaches this return (may include parameters, locals, field reads).
    pub sources: Vec<DataNodeId>,
    /// Confidence of this flow.
    pub confidence: f64,
    /// Provenance: how this flow was determined.
    pub provenance: String,
}

/// Call argument and the upstream nodes that feed into it.
///
/// This links a callsite argument back to its data sources within the
/// calling function — the entry point for summary-bridge interprocedural
/// trace (caller arg → callee param).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CallArgFlow {
    /// The callsite this argument belongs to.
    pub callsite_id: CallsiteId,
    /// Argument index (0-based position in the call argument list).
    pub arg_index: usize,
    /// The call-argument data node.
    pub arg_node_id: DataNodeId,
    /// Upstream data nodes that feed into this argument (within the caller).
    pub sources: Vec<DataNodeId>,
    /// Confidence of this flow.
    pub confidence: f64,
    /// Provenance: how this flow was determined.
    pub provenance: String,
}

impl FunctionSummary {
    /// Quick check: does this summary contain no usable data?
    pub fn is_empty(&self) -> bool {
        self.param_flows.is_empty()
            && self.return_flows.is_empty()
            && self.call_arg_flows.is_empty()
    }
}

impl ParameterFlow {
    /// Create a flow with direct-edge confidence (confirmed Assign/ArgToParam edge).
    pub fn direct(param_id: DataNodeId, param_index: usize, param_name: String) -> Self {
        Self {
            param_id,
            param_index,
            param_name,
            reaches_call_args: vec![],
            reaches_returns: vec![],
            reaches_fields: vec![],
            confidence: 1.0,
            provenance: "intraprocedural_dataflow".to_string(),
        }
    }

    /// Create a flow with BFS-multi-edge confidence.
    pub fn bfs(param_id: DataNodeId, param_index: usize, param_name: String) -> Self {
        Self {
            param_id,
            param_index,
            param_name,
            reaches_call_args: vec![],
            reaches_returns: vec![],
            reaches_fields: vec![],
            confidence: 0.85,
            provenance: "intraprocedural_dataflow".to_string(),
        }
    }
}

impl ReturnFlow {
    /// Create a return flow with direct-edge confidence.
    pub fn direct(return_id: DataNodeId, sources: Vec<DataNodeId>) -> Self {
        Self {
            return_id,
            sources,
            confidence: 1.0,
            provenance: "intraprocedural_dataflow".to_string(),
        }
    }
}

impl CallArgFlow {
    /// Create a call-arg flow with direct-edge confidence.
    pub fn direct(
        callsite_id: CallsiteId,
        arg_index: usize,
        arg_node_id: DataNodeId,
        sources: Vec<DataNodeId>,
    ) -> Self {
        Self {
            callsite_id,
            arg_index,
            arg_node_id,
            sources,
            confidence: 1.0,
            provenance: "intraprocedural_dataflow".to_string(),
        }
    }
}
