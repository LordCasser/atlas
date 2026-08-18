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

use db::{DataflowReader, Store};
use types::dataflow::DataNode;
use types::enums::DataFlowKind;
use types::ids::DataNodeId;
use types::trace::{TraceDiagnostic, TracePath, TracePathStep, TracePoint};

use super::virtual_edges::TraceEdgeProvider;

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
    /// * `max_depth` — maximum number of backward steps.
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
        queue.push_back((sink_node.id, 0));

        let mut farthest_node_id = sink_node.id;
        let mut farthest_depth: usize = 0;
        let mut truncated = false;

        while let Some((current_id, depth)) = queue.pop_front() {
            let edges = store.find_dataflow_edges_by_target(&current_id)?;

            // Inter-procedural: also query virtual edges across call boundaries
            let virtual_edges = if let Some(provider) = edge_provider {
                provider
                    .virtual_incoming(&current_id, store)?
                    .into_iter()
                    .map(|ve| ve.to_dataflow_edge())
                    .collect::<Vec<_>>()
            } else {
                vec![]
            };

            // Collect all candidate edges (real + virtual)
            let mut candidates: Vec<_> = edges
                .iter()
                .filter(|e| should_trace_backward(&e.kind))
                .chain(
                    virtual_edges
                        .iter()
                        .filter(|e| should_trace_backward(&e.kind)),
                )
                .collect();

            // Reads prefer the latest Local/Parameter reaching definition.
            // Writes prefer their explicit value source instead: otherwise a
            // prior Local→Local approximation can hide the RHS that actually
            // produced the new value.
            //
            // Secondary sort (when both sources are Local/Param): prefer the
            // CLOSEST preceding definition (largest start_byte) so the BFS
            // chain hops through all intermediate assignments rather than
            // jumping straight to the earliest definition.
            // Pre-fetch all source data nodes once, then sort from the
            // in-memory map.  This avoids O(n log n) DB queries inside the
            // comparator — each candidate's node is fetched exactly once.
            let data_nodes: HashMap<DataNodeId, DataNode> = candidates
                .iter()
                .filter_map(|e| {
                    store
                        .get_data_node(&e.source)
                        .ok()
                        .flatten()
                        .map(|dn| (e.source, dn))
                })
                .collect();
            let current_is_definition = store.get_data_node(&current_id)?.is_some_and(|node| {
                matches!(
                    node.kind,
                    types::enums::DataNodeKind::Local
                        | types::enums::DataNodeKind::Parameter
                        | types::enums::DataNodeKind::Global
                )
            });

            candidates.sort_by(|a, b| {
                use std::cmp::Ordering;
                let a_dn = data_nodes.get(&a.source);
                let b_dn = data_nodes.get(&b.source);
                let a_local = a_dn
                    .map(|dn| {
                        matches!(
                            dn.kind,
                            types::enums::DataNodeKind::Local
                                | types::enums::DataNodeKind::Parameter
                                | types::enums::DataNodeKind::Global
                        )
                    })
                    .unwrap_or(false);
                let b_local = b_dn
                    .map(|dn| {
                        matches!(
                            dn.kind,
                            types::enums::DataNodeKind::Local
                                | types::enums::DataNodeKind::Parameter
                                | types::enums::DataNodeKind::Global
                        )
                    })
                    .unwrap_or(false);
                match (a_local, b_local) {
                    (true, false) => {
                        if current_is_definition {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    }
                    (false, true) => {
                        if current_is_definition {
                            Ordering::Greater
                        } else {
                            Ordering::Less
                        }
                    }
                    (true, true) => {
                        // Both are Local/Param: sort by source start_byte ASC.
                        // The closest preceding definition (largest start_byte)
                        // is processed LAST and overwrites in predecessors.
                        let a_byte = a_dn.map(|dn| dn.range.start_byte).unwrap_or(0);
                        let b_byte = b_dn.map(|dn| dn.range.start_byte).unwrap_or(0);
                        a_byte.cmp(&b_byte)
                    }
                    _ => {
                        // A linear trace cannot display every operand of an
                        // expression. Prefer state-bearing inputs over
                        // literals so read-modify-write chains follow the
                        // previous value instead of terminating at a constant.
                        let source_priority = |node: Option<&DataNode>| match node.map(|n| n.kind) {
                            Some(
                                types::enums::DataNodeKind::VariableUse
                                | types::enums::DataNodeKind::CallArg
                                | types::enums::DataNodeKind::Field
                                | types::enums::DataNodeKind::Receiver,
                            ) => 3,
                            Some(
                                types::enums::DataNodeKind::Expr
                                | types::enums::DataNodeKind::Return
                                | types::enums::DataNodeKind::CallTarget,
                            ) => 2,
                            Some(types::enums::DataNodeKind::Literal) => 0,
                            Some(_) => 1,
                            None => 0,
                        };
                        source_priority(a_dn)
                            .cmp(&source_priority(b_dn))
                            .then_with(|| {
                                a_dn.map(|dn| dn.range.start_byte)
                                    .unwrap_or(0)
                                    .cmp(&b_dn.map(|dn| dn.range.start_byte).unwrap_or(0))
                            })
                    }
                }
            });

            // ── Phase 1: set predecessors for all viable candidates ────
            // Walking all candidates lets the sort-directed "last wins"
            // strategy select either the closest reaching definition for a
            // read or the explicit value source for a write.
            if !candidates.is_empty() {
                let current_key = hex::encode(current_id.as_bytes());
                for edge in &candidates {
                    let source_key = hex::encode(edge.source.as_bytes());
                    if !visited.contains_key(&source_key) {
                        predecessors.insert(current_key.clone(), (edge.source, edge.kind));
                    }
                }
            }

            // ── Phase 2: enqueue ONLY the highest-priority candidate ────
            // Visiting every candidate in one BFS iteration marks all their
            // sources as "visited" prematurely, blocking later BFS hops.
            // Example: R ← L1, R ← L2, R ← L3 (all Local).
            // If all three are enqueued+visited at depth=1, then when L3
            // is processed at depth 1, L2 and L1 are already visited and
            // cannot serve as predecessors for L3, breaking the chain
            // L1→L2→L3→R.  By enqueuing only the last candidate (L3), we
            // keep L1/L2 available as upstream predecessors.
            if let Some(best_edge) = candidates.last() {
                let source_id = &best_edge.source;
                let source_key = hex::encode(source_id.as_bytes());
                if !visited.contains_key(&source_key) {
                    let new_depth = depth + 1;
                    if new_depth >= max_depth {
                        // Budget exhausted — check if this source has unexplored
                        // predecessors (not just the edge we already followed).
                        let source_edges = store.find_dataflow_edges_by_target(source_id)?;
                        if source_edges.iter().any(|e| should_trace_backward(&e.kind)) {
                            truncated = true;
                            if new_depth > farthest_depth {
                                farthest_depth = new_depth;
                                farthest_node_id = *source_id;
                            }
                        }
                    } else {
                        visited.insert(source_key.clone(), new_depth);
                        queue.push_back((*source_id, new_depth));
                        if new_depth > farthest_depth {
                            farthest_depth = new_depth;
                            farthest_node_id = *source_id;
                        }
                    }
                }
            }
        }

        // Reconstruct path from farthest node to sink
        let mut steps = reconstruct_path(&predecessors, &farthest_node_id, &sink_node.id, store)?;

        // Populate evidence on every step so cross‑file virtual edges
        // carry file‑path attribution (needed by test assertions and
        // agent/AI consumers).
        for step in &mut steps {
            if step.evidence.is_none() {
                step.evidence = build_step_evidence(store, &step.file_id, &step.from_node_id);
            }
        }

        // Resolve the source node as a TracePoint
        let source_node = store.get_data_node(&farthest_node_id)?.unwrap_or_else(|| {
            // Fallback: create a minimal data node for display
            DataNode::parameter(
                farthest_node_id,
                sink_node.file_id,
                None,
                None,
                "unknown",
                sink_node.range,
            )
        });
        let source_point = TracePoint {
            reference: None,
            resolved_symbol: None,
            data_node: Some(source_node.clone()),
            incoming: vec![],
            outgoing: vec![],
            binding: None,
            binding_use: None,
            scope: None,
            callsite: None,
            file_id: source_node.file_id,
            line: source_node.range.start_line + 1,
            column: source_node.range.start_column + 1,
            capability: sink_point.capability.clone(),
            partial_result: false,
            diagnostics: vec![],
        };

        let mut diagnostics = Vec::new();
        let partial = truncated;
        if truncated {
            diagnostics.push(
                TraceDiagnostic::warning(&format!(
                    "Backward trace truncated at max_depth={max_depth} (reached depth {farthest_depth})"
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
            lazy_summary: None,
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
            | DataFlowKind::ArgToCall
            | DataFlowKind::ArgToParam
            | DataFlowKind::ReturnValue
            | DataFlowKind::ReturnToCall
            | DataFlowKind::ReceiverToThis
            | DataFlowKind::StateFlow
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
    let mut current = *sink_node_id;

    while &current != farthest_node_id {
        let key = hex::encode(current.as_bytes());
        match predecessors.get(&key) {
            Some((pred_id, kind)) => {
                raw_steps.push((*pred_id, current, *kind));
                current = *pred_id;
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
            types::ids::FileId::generate("unknown")
        };

        let range = store
            .get_data_node(&from)?
            .map(|n| n.range)
            .or_else(|| store.get_data_node(&to).ok().flatten().map(|n| n.range));

        steps.push(TracePathStep::new(
            idx as u32,
            from,
            to,
            kind,
            kind_description(&kind),
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
        DataFlowKind::ArgToCall => "call argument → call target",
        DataFlowKind::ArgToParam => "argument → parameter (cross-function)",
        DataFlowKind::ReturnValue => "expression → return",
        DataFlowKind::ReturnToCall => "return → callsite (cross-function)",
        DataFlowKind::ReceiverToThis => "receiver → self",
        DataFlowKind::StateFlow => "framework state flow",
        DataFlowKind::Phi => "phi (control-flow merge)",
    }
}

/// Build an [`Evidence`] from file metadata and a data node.
///
/// Used to populate step-level evidence for trace-path display and
/// assertion verification.  Reads file path from the store and node
/// name from the data node.
fn build_step_evidence(
    store: &Store,
    file_id: &types::ids::FileId,
    node_id: &DataNodeId,
) -> Option<types::trace::Evidence> {
    let file_path = store.get_file(file_id).ok().flatten().map(|fi| fi.path)?;
    let data_node = store.get_data_node(node_id).ok().flatten();
    let symbol_name = data_node.as_ref().and_then(|n| n.name.clone());
    Some(types::trace::Evidence {
        file_path,
        snippet: None,
        symbol_name,
    })
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use types::enums::DataFlowKind;

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
            DataFlowKind::ArgToCall,
            DataFlowKind::ArgToParam,
            DataFlowKind::ReturnValue,
            DataFlowKind::StateFlow,
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
        assert!((compute_confidence(30, false) - 0.3).abs() < 0.01);
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
            compute_confidence(30, true) >= 0.1,
            "confidence floor is 0.1"
        );
    }
}
