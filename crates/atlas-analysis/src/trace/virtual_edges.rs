//! Virtual edges for inter-procedural dataflow tracing.
//!
//! When the backward slicer hits a function boundary (parameter node at the
//! top of a function, or call-return node with an associated [`Callsite`]),
//! it needs to "jump" across the call boundary to continue tracing into the
//! caller or callee.  These cross-boundary jumps are modelled as virtual
//! [`TraceEdge`]s provided by a [`TraceEdgeProvider`].
//!
//! ## Bridge types
//!
//! || Direction || From || To || When ||
//! || Backward (caller arg → callee param) || CallArg DataNode in caller || Parameter DataNode in callee || Slicer reaches a Parameter node with known callers ||
//! || Backward (callee return → caller result) || Return DataNode in callee || Expr/CallResult DataNode in caller || Slicer reaches a call-result node with known callee ||
//!
//! The [`SummaryEdgeProvider`] uses [`super::summary::FunctionSummary`] to
//! bridge these gaps.  When no summary exists yet it falls back to
//! direct-join heuristics (match callsite callee symbol → callee params;
//! match call-arg DataNode → callee param by index).

use atlas_db::Store;
use atlas_db::TraceStore;
use atlas_types::dataflow::DataFlowEdge;
use atlas_types::enums::{DataFlowKind, DataNodeKind};
use atlas_types::ids::DataNodeId;

// ---------------------------------------------------------------------------
// TraceEdge — a cross-boundary dataflow connection
// ---------------------------------------------------------------------------

/// A virtual edge that connects data nodes across function boundaries.
///
/// Unlike [`DataFlowEdge`] which stays within a single function, `TraceEdge`
/// bridges caller-callee or callee-caller transitions needed for
/// inter-procedural backward tracing.
#[derive(Debug, Clone)]
pub struct TraceEdge {
    /// Source of the data flow (caller-side for backward tracing).
    pub source_id: DataNodeId,
    /// Target of the data flow (callee-side for backward tracing).
    pub target_id: DataNodeId,
    /// Edge kind — typically `ArgToParam` or `ReturnToCall`.
    pub kind: DataFlowKind,
    /// Confidence (0.0–1.0).  Virtual edges have lower confidence than
    /// intra-procedural dataflow edges.
    pub confidence: f64,
    /// Human-readable provenance describing how this edge was inferred.
    pub provenance: String,
}

// ---------------------------------------------------------------------------
// TraceEdgeProvider trait
// ---------------------------------------------------------------------------

/// Provider of virtual inter-procedural trace edges.
///
/// Implementations bridge the gap between intra-procedural dataflow (which
/// stays within a single function) and cross-function tracing.
pub trait TraceEdgeProvider: Send + Sync {
    /// Return virtual edges whose **target** is the given node.  For backward
    /// tracing, these are edges *into* this node from across a call boundary.
    fn virtual_incoming(
        &self,
        target_id: &DataNodeId,
        store: &dyn TraceStore,
    ) -> anyhow::Result<Vec<TraceEdge>>;
}

// ---------------------------------------------------------------------------
// SummaryEdgeProvider — bridges using FunctionSummary
// ---------------------------------------------------------------------------

/// Bridges call boundaries using [`FunctionSummary`] and direct DB joins.
///
/// ## Strategy (in priority order)
///
/// 1. **Parameter node** → find callers via `callsites_by_callee`, match
///    each caller's call-arg DataNode to this parameter by arg_index.
/// 2. **CallReturn / Expr nodes with callsite_id** → find the callee
///    function, look up its summary, connect each ReturnFlow source back
///    to this call-result node.
/// 3. **Direct callsite join** — when summary is unavailable, match
///    caller call-arg DataNodes to callee parameters using the existing
///    `callsite_id` on DataNodes and the callsite's callee symbol.
pub struct SummaryEdgeProvider;

impl TraceEdgeProvider for SummaryEdgeProvider {
    fn virtual_incoming(
        &self,
        target_id: &DataNodeId,
        store: &dyn TraceStore,
    ) -> anyhow::Result<Vec<TraceEdge>> {
        let target_node = match store.get_data_node(target_id)? {
            Some(n) => n,
            None => return Ok(vec![]),
        };

        let mut edges: Vec<TraceEdge> = Vec::new();

        match target_node.kind {
            // ── Parameter: find callers that pass arguments ──
            DataNodeKind::Parameter => {
                // Find the function symbol this parameter belongs to
                let function_id = match &target_node.function_id {
                    Some(fid) => fid.clone(),
                    None => return Ok(vec![]),
                };

                // Find all callsites targeting this function
                let callers = store.find_callsites_by_callee(&function_id)?;

                // Hoisted: load callee parameters once (not per callsite/arg).
                let callee_params = store.find_data_nodes_by_function(&function_id)?;
                let param_index = callee_params
                    .iter()
                    .filter(|dn| dn.kind == DataNodeKind::Parameter)
                    .position(|dn| &dn.id == target_id);

                for cs in &callers {
                    for (arg_idx, arg) in cs.args.iter().enumerate() {
                        let arg_dn_id = match &arg.data_node_id {
                            Some(dn_id) => dn_id,
                            None => continue,
                        };

                        if let Some(param_idx) = param_index {
                            if arg_idx == param_idx {
                                edges.push(TraceEdge {
                                    source_id: arg_dn_id.clone(),
                                    target_id: target_id.clone(),
                                    kind: DataFlowKind::ArgToParam,
                                    confidence: 0.67,
                                    provenance: format!(
                                        "caller arg[{}] at callsite {} → callee param[{}]",
                                        arg_idx,
                                        hex::encode(cs.id.as_bytes()),
                                        param_idx,
                                    ),
                                });
                            }
                        }
                    }
                }
            }

            // ── CallReturn-like: find the callee and connect its returns ──
            DataNodeKind::CallReturn | DataNodeKind::Expr => {
                let callsite_id = match &target_node.callsite_id {
                    Some(csid) => csid,
                    None => return Ok(vec![]),
                };

                // Find the callsite → get the callee symbol
                let callsites = store.find_callsites_by_id(callsite_id)?;
                let callee_sym_id = match callsites.first().and_then(|cs| cs.callee.as_ref()) {
                    Some(sid) => sid.clone(),
                    None => return Ok(vec![]),
                };

                // Get the callee symbol to obtain file_id for summary
                let callee_sym = match store.find_symbol_by_id(&callee_sym_id)? {
                    Some(s) => s,
                    None => return Ok(vec![]),
                };

                // Try summary-based bridge first
                if let Ok(summary) = crate::summary::SummaryBuilder::build(
                    store,
                    &callee_sym_id,
                    Some((callee_sym.range.start_byte, callee_sym.range.end_byte)),
                ) {
                    for rf in &summary.return_flows {
                        for src_id in &rf.sources {
                            edges.push(TraceEdge {
                                source_id: src_id.clone(),
                                target_id: target_id.clone(),
                                kind: DataFlowKind::ReturnToCall,
                                confidence: rf.confidence * 0.85, // cross-boundary penalty
                                provenance: format!(
                                    "callee return {} → call site {} (summary bridge)",
                                    hex::encode(rf.return_id.as_bytes()),
                                    hex::encode(callsite_id.as_bytes()),
                                ),
                            });
                        }
                    }
                }
            }

            _ => {}
        }

        Ok(edges)
    }
}

// ---------------------------------------------------------------------------
// Composite provider — chain multiple providers
// ---------------------------------------------------------------------------

/// Chains multiple [`TraceEdgeProvider`]s together, merging results.
pub struct CompositeProvider {
    providers: Vec<Box<dyn TraceEdgeProvider>>,
}

impl CompositeProvider {
    /// Create a composite provider from one or more providers.
    pub fn new(providers: Vec<Box<dyn TraceEdgeProvider>>) -> Self {
        Self { providers }
    }
}

impl TraceEdgeProvider for CompositeProvider {
    fn virtual_incoming(
        &self,
        target_id: &DataNodeId,
        store: &dyn TraceStore,
    ) -> anyhow::Result<Vec<TraceEdge>> {
        let mut all_edges: Vec<TraceEdge> = Vec::new();
        for p in &self.providers {
            let edges = p.virtual_incoming(target_id, store)?;
            all_edges.extend(edges);
        }
        Ok(all_edges)
    }
}

// ---------------------------------------------------------------------------
// Helper — convert TraceEdge → DataFlowEdge for slicer compatibility
// ---------------------------------------------------------------------------

impl TraceEdge {
    /// Convert this virtual edge into a synthetic [`DataFlowEdge`] so the
    /// slicer can process it alongside real intra-procedural edges.
    pub fn to_dataflow_edge(&self) -> DataFlowEdge {
        DataFlowEdge {
            id: atlas_types::ids::DataFlowEdgeId::generate(
                &self.source_id,
                &self.target_id,
                self.kind.as_str(),
            ),
            source: self.source_id.clone(),
            target: self.target_id.clone(),
            kind: self.kind.clone(),
            location: atlas_types::structs::TextRange {
                start_byte: 0,
                end_byte: 0,
                start_line: 0,
                start_column: 0,
                end_line: 0,
                end_column: 0,
            },
            confidence: self.confidence,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_db::Store;

    #[test]
    fn provider_returns_empty_on_missing_data() -> anyhow::Result<()> {
        use atlas_types::ids::FileId;

        let store = Store::open_in_memory()?;
        store.init_schema()?;
        let provider = SummaryEdgeProvider;

        // Parameter without function_id or callers — DB has nothing
        let file_id = FileId::generate("test.ts");
        let param_id = DataNodeId::generate(&file_id, None, "param", None, None, 0);
        let edges = provider.virtual_incoming(&param_id, &store)?;
        assert!(edges.is_empty(), "non-existent param should yield no edges");

        // CallReturn without callsite_id
        let cr_id = DataNodeId::generate(&file_id, None, "call_return", None, None, 0);
        let edges = provider.virtual_incoming(&cr_id, &store)?;
        assert!(
            edges.is_empty(),
            "non-existent call return should yield no edges"
        );

        Ok(())
    }
}
