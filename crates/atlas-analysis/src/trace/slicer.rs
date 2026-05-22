//! Backward dataflow slicing — reconstruct how a value reaches a point.
//!
//! The slicer walks backward through dataflow edges from a sink [`DataNode`]
//! to find all upstream data sources.  It produces a [`TracePath`] showing
//! how a value reaches the given position.
//!
//! # Algorithm
//!
//! 1. Start from the sink node in the [`TracePoint`].
//! 2. BFS backward through incoming dataflow edges (`Assign`, `Read`, `Write`,
//!    `FieldLoad`, `ArgToParam`) until we hit a terminal node (parameter,
//!    literal, global) or exhaust the search depth.
//! 3. Reconstruct the forward path from the farthest source to the sink.
//!
//! # Limitations
//!
//! - The slicer is intra-procedural: it follows dataflow edges within a
//!   function but does not cross call boundaries.
//! - The "source" is the farthest node we can reach backward — it may not
//!   always be a true program input.

use std::collections::{HashMap, VecDeque};

use atlas_db::{DataflowReader, Store};
use atlas_types::dataflow::DataNode;
use atlas_types::enums::DataFlowKind;
use atlas_types::ids::DataNodeId;
use atlas_types::trace::{TraceDiagnostic, TracePath, TracePathStep, TracePoint};

use super::virtual_edges::TraceEdgeProvider;

/// Default maximum depth for backward dataflow slicing (used by callers
/// and tests; current call sites pass `max_depth` explicitly).
#[allow(dead_code)]
pub const DEFAULT_MAX_DEPTH: usize = 30;

/// Produces a backward dataflow trace from a [`TracePoint`].
pub struct Slicer;

impl Slicer {
    /// Slice backward from the data node in `sink_point`, producing a
    /// [`TracePath`] that shows how the value reached this position.
    ///
    /// Returns `Ok(None)` if `sink_point` has no data node (nothing to trace).
    ///
    /// # Arguments
    ///
    /// * `store` — the Atlas database for querying dataflow edges and nodes.
    /// * `sink_point` — the user-chosen position to trace from.
    /// * `max_depth` — maximum number of backward steps (default: [`DEFAULT_MAX_DEPTH`]).
    pub fn slice(
        store: &Store,
        sink_point: &TracePoint,
        max_depth: usize,
        edge_provider: Option<&dyn TraceEdgeProvider>,
    ) -> anyhow::Result<Option<TracePath>> {
        let sink_node = match &sink_point.data_node {
            Some(dn) => dn,
            None => return Ok(None),
        };

        // BFS backward: node_id → predecessor info
        let mut predecessors: HashMap<String, (DataNodeId, DataFlowKind)> = HashMap::new();
        let mut visited: HashMap<String, usize> = HashMap::new(); // node_id hex → depth
        let mut queue: VecDeque<(DataNodeId, usize)> = VecDeque::new();

        let sink_key = hex::encode(sink_node.id.as_bytes());
        visited.insert(sink_key.clone(), 0);
        queue.push_back((sink_node.id.clone(), 0));

        let mut farthest_node_id = sink_node.id.clone();
        let mut farthest_depth: usize = 0;
        let mut truncated = false;

        while let Some((current_id, depth)) = queue.pop_front() {
            let edges = store
                .find_dataflow_edges_by_target(&current_id)
                .unwrap_or_default();

            // Inter-procedural: also query virtual edges across call boundaries
            let virtual_edges = if let Some(provider) = edge_provider {
                provider
                    .virtual_incoming(&current_id, store)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|ve| ve.to_dataflow_edge())
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };

            // Process both real and virtual edges
            for edge in edges.iter().chain(virtual_edges.iter()) {
                // Only follow dataflow edges that represent value movement
                if !should_trace_backward(&edge.kind) {
                    continue;
                }

                let source_id = &edge.source;
                let source_key = hex::encode(source_id.as_bytes());

                if !visited.contains_key(&source_key) {
                    let new_depth = depth + 1;
                    if new_depth >= max_depth {
                        // Budget exhausted — check if this source has unexplored
                        // predecessors (not just the edge we already followed).
                        let source_edges = store
                            .find_dataflow_edges_by_target(source_id)
                            .unwrap_or_default();
                        if source_edges.iter().any(|e| should_trace_backward(&e.kind)) {
                            truncated = true;
                            // Track this frontier node as the farthest known point.
                            if new_depth > farthest_depth {
                                farthest_depth = new_depth;
                                farthest_node_id = source_id.clone();
                            }
                        }
                        continue;
                    }
                    let current_key = hex::encode(current_id.as_bytes());
                    visited.insert(source_key.clone(), new_depth);
                    // Store current→source so reconstruct_path can walk
                    // backward from sink through each predecessor.
                    predecessors.insert(current_key, (source_id.clone(), edge.kind.clone()));
                    queue.push_back((source_id.clone(), new_depth));

                    if new_depth > farthest_depth {
                        farthest_depth = new_depth;
                        farthest_node_id = source_id.clone();
                    }
                }
            }
        }

        // Reconstruct path from farthest node to sink
        let steps = reconstruct_path(&predecessors, &farthest_node_id, &sink_node.id, store)?;

        // Resolve the source node as a TracePoint
        let source_node = store.get_data_node(&farthest_node_id)?.unwrap_or_else(|| {
            // Fallback: create a minimal data node for display
            DataNode::parameter(
                farthest_node_id.clone(),
                sink_node.file_id.clone(),
                None,
                None,
                "unknown",
                sink_node.range.clone(),
            )
        });
        let source_point = TracePoint {
            reference: None,
            resolved_symbol: None,
            data_node: Some(source_node),
            incoming: vec![],
            outgoing: vec![],
            binding: None,
            binding_use: None,
            scope: None,
            callsite: None,
            file_id: sink_point.file_id.clone(),
            line: sink_point.line,
            column: sink_point.column,
            capability: sink_point.capability.clone(),
            partial_result: false,
            diagnostics: vec![],
        };

        let mut diagnostics = Vec::new();
        let partial = truncated;
        if truncated {
            diagnostics.push(
                TraceDiagnostic::warning(&format!(
                    "Backward trace truncated at max_depth={} (reached depth {})",
                    max_depth, farthest_depth
                ))
                .with_code("max_depth_truncated"),
            );
        }

        Ok(Some(TracePath {
            source: source_point,
            steps,
            sink: sink_point.clone(),
            confidence: compute_confidence(farthest_depth, truncated),
            nodes_visited: visited.len(),
            max_depth_reached: farthest_depth,
            capability: sink_point.capability.clone(),
            partial_result: partial,
            diagnostics,
        }))
    }
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Decide whether a dataflow edge kind should be followed backward by the
/// slicer.  We trace through assignment, read/write, field access, and
/// argument-to-parameter mappings.  Structural edges (like `Contains`) and
/// opaque edges (like `Unknown`) are skipped.
fn should_trace_backward(kind: &DataFlowKind) -> bool {
    matches!(
        kind,
        DataFlowKind::Assign
            | DataFlowKind::Read
            | DataFlowKind::Write
            | DataFlowKind::FieldLoad
            | DataFlowKind::FieldStore
            | DataFlowKind::ArgToParam
            | DataFlowKind::ReturnToCall
            | DataFlowKind::ReceiverToThis
    )
}

/// Reconstruct a forward path from `farthest_node_id` to `sink_node_id` by
/// walking the predecessor chain backward.
fn reconstruct_path(
    predecessors: &HashMap<String, (DataNodeId, DataFlowKind)>,
    farthest_node_id: &DataNodeId,
    sink_node_id: &DataNodeId,
    store: &impl DataflowReader,
) -> anyhow::Result<Vec<TracePathStep>> {
    // Walk from sink backward to farthest, collecting steps in reverse
    let mut raw_steps: Vec<(DataNodeId, DataNodeId, DataFlowKind)> = Vec::new();
    let mut current = sink_node_id.clone();

    while &current != farthest_node_id {
        let key = hex::encode(current.as_bytes());
        match predecessors.get(&key) {
            Some((pred_id, kind)) => {
                raw_steps.push((pred_id.clone(), current.clone(), kind.clone()));
                current = pred_id.clone();
            }
            None => break,
        }
    }

    // Reverse to get source→sink order
    raw_steps.reverse();

    let mut steps = Vec::new();
    for (idx, (from, to, kind)) in raw_steps.into_iter().enumerate() {
        let file_id = if let Some(node) = store.get_data_node(&from)? {
            node.file_id
        } else if let Some(node) = store.get_data_node(&to)? {
            node.file_id
        } else {
            // Fallback — use an empty file ID (should not normally happen)
            atlas_types::ids::FileId::generate("unknown")
        };

        let range = store
            .get_data_node(&from)?
            .map(|n| n.range)
            .or_else(|| store.get_data_node(&to).ok().flatten().map(|n| n.range));

        steps.push(TracePathStep::new(
            idx as u32,
            from,
            to,
            kind.clone(),
            &format!("{}", kind_description(&kind)),
            file_id,
            range,
        ));
    }

    Ok(steps)
}

/// Compute path confidence based on depth and truncation status.
/// Shorter paths are more confident; truncated paths get a penalty.
fn compute_confidence(depth: usize, truncated: bool) -> f64 {
    if depth == 0 {
        1.0
    } else {
        let base = (1.0 - depth as f64 * 0.033).max(0.3);
        if truncated {
            (base - 0.2).max(0.1)
        } else {
            base
        }
    }
}

/// Human-readable description of a dataflow edge kind for trace steps.
fn kind_description(kind: &DataFlowKind) -> &'static str {
    match kind {
        DataFlowKind::Assign => "assignment",
        DataFlowKind::Read => "read",
        DataFlowKind::Write => "write",
        DataFlowKind::FieldLoad => "field access",
        DataFlowKind::FieldStore => "field store",
        DataFlowKind::ArgToParam => "argument → parameter",
        DataFlowKind::ReturnToCall => "return → callsite",
        DataFlowKind::ReceiverToThis => "receiver → self",
        DataFlowKind::Phi => "phi (control-flow merge)",
    }
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use atlas_types::enums::DataFlowKind;

    #[test]
    fn should_trace_backward_assign() {
        assert!(should_trace_backward(&DataFlowKind::Assign));
    }

    #[test]
    fn should_not_trace_phi() {
        // Phi edges represent control-flow merges (e.g., if/else join).
        // The slicer currently does NOT follow them — it stays on value-flow
        // edges only.  This may change when CFG awareness is added.
        assert!(!should_trace_backward(&DataFlowKind::Phi));
    }

    #[test]
    fn kind_description_is_non_empty() {
        for kind in &[
            DataFlowKind::Assign,
            DataFlowKind::Read,
            DataFlowKind::FieldLoad,
            DataFlowKind::ArgToParam,
            DataFlowKind::Phi,
        ] {
            assert!(!kind_description(kind).is_empty());
        }
    }

    #[test]
    fn compute_confidence_decays_with_depth() {
        assert!((compute_confidence(0, false) - 1.0).abs() < 0.01);
        assert!(compute_confidence(5, false) < 1.0);
        assert!(compute_confidence(5, false) > compute_confidence(15, false));
        assert!((compute_confidence(DEFAULT_MAX_DEPTH, false) - 0.3).abs() < 0.01);
    }

    #[test]
    fn compute_confidence_truncated_penalty() {
        // Non-truncated at depth 10 — should be higher than truncated at same depth
        assert!(
            compute_confidence(10, false) > compute_confidence(10, true),
            "truncated paths should have lower confidence"
        );
        // Truncated at max depth — should not go below floor 0.1
        assert!(
            compute_confidence(DEFAULT_MAX_DEPTH, true) >= 0.1,
            "confidence floor is 0.1"
        );
    }
}
