//! Backward dataflow slicing — reconstruct how a value reaches a point.
//!
//! The slicer walks backward through dataflow edges from a sink [`DataNode`]
//! to display one recorded path. Alternative edges and analysis boundaries
//! remain explicit diagnostics with positions for further investigation.
//!
//! # Algorithm
//!
//! 1. Start from the sink node in the [`TracePoint`].
//! 2. Select a deterministic unvisited predecessor at each step, preserving
//!    omitted alternatives, cycles and depth limits in diagnostics.
//! 3. Reconstruct the displayed source-to-sink path, without claiming that its
//!    endpoint is the only origin or that the program analysis is complete.
//!
//! # Limitations
//!
//! - Recorded callsite context restricts call boundaries, but does not prove
//!   runtime feasibility or exhaustive dataflow coverage.
//! - A single path cannot represent all origins or loop iterations.

use std::collections::{BTreeSet, HashMap, HashSet};

use db::Store;
use types::dataflow::DataNode;
use types::enums::DataFlowKind;
use types::ids::{CallsiteId, DataNodeId};
use types::trace::{TraceDiagnostic, TracePath, TracePathStep, TracePoint};

use super::call_context::{CallContexts, Candidate, Transition};
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
        excluded_returns: &BTreeSet<(DataNodeId, Vec<CallsiteId>)>,
    ) -> anyhow::Result<Option<TracePath>> {
        let sink_node = match &sink_point.data_node {
            Some(dn) => dn,
            None => return Ok(None),
        };

        // A node can occur in several invocations. Keep the displayed chain
        // directly instead of overwriting predecessors keyed only by node ID.
        let mut steps = Vec::new();
        let mut current = sink_node.clone();
        let mut context = sink_point.call_context.clone();
        let mut contexts = CallContexts::new(store);
        let mut visited = HashSet::from([(current.id, context.clone())]);
        let mut truncated = false;
        let mut diagnostics = sink_point.diagnostics.clone();
        let mut file_diagnostics = HashMap::new();
        let mut unit_diagnostics = HashMap::new();
        let mut reported_extraction = HashSet::new();

        loop {
            let current_node = &current;
            let current_id = current.id;
            if !contexts.valid_for(current_node, &context)? {
                diagnostics.push(frontier_diagnostic(
                    "trace_call_context_unavailable",
                    "The requested invocation context is not established for this node",
                    current_node,
                    &context,
                    &[],
                    &HashMap::new(),
                ));
                break;
            }
            if let std::collections::hash_map::Entry::Vacant(entry) =
                file_diagnostics.entry(current_node.file_id)
            {
                entry.insert(store.file_diagnostics(&current_node.file_id)?);
            }
            let unit_key = (current_node.file_id, current_node.function_id);
            if let std::collections::hash_map::Entry::Vacant(entry) =
                unit_diagnostics.entry(unit_key)
            {
                entry.insert(
                    store.unit_dataflow_diagnostics(
                        &current_node.file_id,
                        current_node.function_id,
                    )?,
                );
            }
            for diagnostic in file_diagnostics[&current_node.file_id]
                .iter()
                .chain(&unit_diagnostics[&unit_key])
            {
                let affects_node = diagnostic.range.is_none_or(|range| {
                    if range.start_byte == range.end_byte {
                        // A missing token can sit at the expression boundary.
                        current_node.range.start_byte <= range.start_byte
                            && range.start_byte <= current_node.range.end_byte
                    } else {
                        range.start_byte < current_node.range.end_byte
                            && current_node.range.start_byte < range.end_byte
                    }
                });
                if diagnostic.level != types::DiagnosticLevel::Info
                    && affects_node
                    && reported_extraction
                        .insert((current_node.file_id, serde_json::to_string(diagnostic)?))
                {
                    diagnostics.push(TraceDiagnostic::warning(&diagnostic.message)
                        .with_code("trace_extraction_limit")
                        .with_detail(serde_json::json!({"file_id": current_node.file_id, "location": diagnostic.range}).to_string()));
                }
            }
            let mut incoming: Vec<_> = store
                .find_dataflow_edges_by_target(&current_id)?
                .into_iter()
                .filter(|e| should_trace_backward(&e.kind))
                .map(|edge| Candidate {
                    edge,
                    callsite_id: None,
                    virtual_edge: false,
                    context: None,
                    excluded: false,
                })
                .collect();
            if let Some(provider) = edge_provider {
                incoming.extend(
                    provider
                        .virtual_incoming(&current_id, store)?
                        .into_iter()
                        .filter(|e| should_trace_backward(&e.kind))
                        .map(|edge| Candidate {
                            edge: edge.to_dataflow_edge(),
                            callsite_id: edge.callsite_id,
                            virtual_edge: true,
                            context: None,
                            excluded: false,
                        }),
                );
            }

            if current_node.kind == types::DataNodeKind::CallOutput
                && !incoming
                    .iter()
                    .any(|e| e.kind == DataFlowKind::WritebackToCall)
            {
                let calls = current_node
                    .callsite_id
                    .map(|id| store.find_callsites_by_id(&id))
                    .transpose()?
                    .unwrap_or_default();
                diagnostics.push(TraceDiagnostic::warning(
                    "This binding was exposed as a call argument. The call's writeback or preservation of its prior value is not established; inspect this argument and the selected callee before continuing")
                    .with_code("trace_call_output_unavailable")
                    .with_detail(serde_json::json!({
                        "at": {"file_id": current_node.file_id, "location": current_node.range},
                        "calls": calls.iter().take(1).map(|call| serde_json::json!({"file_id": current_node.file_id, "location": call.range})).collect::<Vec<_>>()
                    }).to_string()));
                break;
            }
            if current_node.kind == types::DataNodeKind::CallReturn
                && !incoming
                    .iter()
                    .any(|edge| edge.kind == DataFlowKind::ReturnToCall)
            {
                diagnostics.push(frontier_diagnostic(
                    "trace_call_result_unavailable",
                    "No supported result source is available for this invocation or construction; evaluating its operands does not establish the result value",
                    current_node,
                    &context,
                    &[],
                    &HashMap::new(),
                ));
            }

            // Reads prefer the latest Local/Parameter reaching definition.
            // Writes prefer their explicit value source instead: otherwise a
            // prior Local→Local approximation can hide the RHS that actually
            // produced the new value.
            //
            // Secondary sort (when both sources are Local/Param): prefer the
            // CLOSEST preceding definition (largest start_byte) so the BFS
            // chain prefers intermediate assignments rather than
            // jumping straight to the earliest definition.
            // Pre-fetch all source data nodes once, then sort from the
            // in-memory map.  This avoids O(n log n) DB queries inside the
            // comparator — each candidate's node is fetched exactly once.
            let mut data_nodes: HashMap<DataNodeId, DataNode> = HashMap::new();
            for edge in &mut incoming {
                if let Some(node) = store.get_data_node(&edge.source)? {
                    match contexts.transition(edge, current_node, &node, &context)? {
                        Transition::Follow(next) => edge.context = Some(next),
                        Transition::Excluded => edge.excluded = true,
                        Transition::Unavailable => {}
                    }
                    data_nodes.insert(edge.source, node);
                }
            }
            let mut candidates: Vec<_> = incoming.iter().collect();
            // At a recorded store, FieldStore describes the assigned input;
            // FieldLoad describes the destination's receiver projection. The
            // latter is useful navigation context, not another value written
            // by this assignment. Keep it located without following it as the
            // stored value (even if the actual value node is unavailable).
            // A separate field read still needs its own memory/alias analysis;
            // this does not join stores and reads by their access-path spelling.
            if current_node.kind == types::DataNodeKind::Field
                && incoming.iter().any(|e| e.kind == DataFlowKind::FieldStore)
            {
                let receivers: Vec<_> = candidates
                    .iter()
                    .copied()
                    .filter(|e| e.kind == DataFlowKind::FieldLoad)
                    .collect();
                if !receivers.is_empty() {
                    diagnostics.push(frontier_diagnostic(
                        "trace_field_receiver_context",
                        "These recorded receiver projections describe the store's object context, not its assigned value; object identity and later field reads require separate investigation",
                        current_node,
                        &context,
                        &receivers,
                        &data_nodes,
                    ));
                    candidates.retain(|e| e.kind != DataFlowKind::FieldLoad);
                }
            }
            let missing: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|e| !data_nodes.contains_key(&e.source))
                .collect();
            if !missing.is_empty() {
                diagnostics.push(frontier_diagnostic(
                    "trace_data_node_missing",
                    "An incoming edge references a missing data node",
                    current_node,
                    &context,
                    &missing,
                    &data_nodes,
                ));
            }
            candidates.retain(|e| data_nodes.contains_key(&e.source));
            let current_is_definition = {
                let node = current_node;
                matches!(
                    node.kind,
                    types::enums::DataNodeKind::Local
                        | types::enums::DataNodeKind::Parameter
                        | types::enums::DataNodeKind::Global
                )
            };

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
                                | types::enums::DataNodeKind::CallOutput
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
                                | types::enums::DataNodeKind::CallOutput
                        )
                    })
                    .unwrap_or(false);
                let order = match (a_local, b_local) {
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
                        // The last available candidate is selected below.
                        let a_byte = a_dn.map(|dn| dn.range.start_byte).unwrap_or(0);
                        let b_byte = b_dn.map(|dn| dn.range.start_byte).unwrap_or(0);
                        a_byte.cmp(&b_byte)
                    }
                    _ => {
                        // A linear trace cannot display every operand of an
                        // expression. Prefer a resolved callee return over
                        // syntactic operands, then state-bearing inputs over
                        // literals so read-modify-write chains follow the
                        // previous value instead of a constant.
                        let source_priority = |node: Option<&DataNode>| match node.map(|n| n.kind) {
                            Some(
                                types::enums::DataNodeKind::VariableUse
                                | types::enums::DataNodeKind::CallArg
                                | types::enums::DataNodeKind::Field
                                | types::enums::DataNodeKind::Receiver,
                            ) => 3,
                            Some(
                                types::enums::DataNodeKind::Expr
                                | types::enums::DataNodeKind::Return,
                            ) => 2,
                            Some(types::enums::DataNodeKind::Literal) => 0,
                            Some(_) => 1,
                            None => 0,
                        };
                        let edge_priority = |kind: DataFlowKind| match kind {
                            DataFlowKind::ReturnToCall
                            | DataFlowKind::WritebackToCall
                            | DataFlowKind::StateFlow => 1,
                            _ => 0,
                        };
                        edge_priority(a.kind)
                            .cmp(&edge_priority(b.kind))
                            .then_with(|| source_priority(a_dn).cmp(&source_priority(b_dn)))
                            .then_with(|| {
                                a_dn.map(|dn| dn.range.start_byte)
                                    .unwrap_or(0)
                                    .cmp(&b_dn.map(|dn| dn.range.start_byte).unwrap_or(0))
                            })
                    }
                };
                order
                    .then_with(|| a.source.cmp(&b.source))
                    .then_with(|| a.kind.as_str().cmp(b.kind.as_str()))
                    .then_with(|| a.callsite_id.cmp(&b.callsite_id))
            });
            candidates.dedup_by_key(|edge| (edge.source, edge.target, edge.kind, edge.callsite_id));

            let virtual_refs: Vec<_> = candidates
                .iter()
                .copied()
                .filter(|e| e.virtual_edge)
                .collect();
            if !virtual_refs.is_empty() {
                diagnostics.push(frontier_diagnostic("trace_virtual_boundary", "Recorded call/state joins do not establish runtime execution or exhaustive value coverage", current_node, &context, &virtual_refs, &data_nodes));
            }
            for (excluded, code, message) in [
                (
                    true,
                    "trace_call_context_excluded",
                    "These incoming calls belong to other invocations of the callee",
                ),
                (
                    false,
                    "trace_call_context_unavailable",
                    "The call boundary or source invocation is not established; inspect its source before continuing",
                ),
            ] {
                let limited: Vec<_> = candidates
                    .iter()
                    .copied()
                    .filter(|e| e.context.is_none() && e.excluded == excluded)
                    .collect();
                if !limited.is_empty() {
                    diagnostics.push(frontier_diagnostic(
                        code,
                        message,
                        current_node,
                        &context,
                        &limited,
                        &data_nodes,
                    ));
                }
            }
            // Exclusions are supplied with semantic evidence by the local
            // investigation and apply to one invocation, never a shared node.
            let contradicts_return = |edge: &&Candidate| {
                matches!(
                    edge.kind,
                    DataFlowKind::ReturnToCall | DataFlowKind::WritebackToCall
                ) && edge
                    .context
                    .as_ref()
                    .is_some_and(|ctx| excluded_returns.contains(&(edge.source, ctx.clone())))
            };
            let excluded: Vec<_> = candidates
                .iter()
                .copied()
                .filter(contradicts_return)
                .collect();
            if !excluded.is_empty() {
                diagnostics.push(frontier_diagnostic(
                    "trace_return_condition_excluded",
                    "These exits conflict with the return condition established for this invocation; the underlying facts remain available outside this condition scope",
                    current_node, &context, &excluded, &data_nodes,
                ));
                candidates.retain(|edge| !contradicts_return(edge));
            }
            if candidates.is_empty() {
                if !matches!(
                    current_node.kind,
                    types::DataNodeKind::Literal
                        | types::DataNodeKind::Parameter
                        | types::DataNodeKind::Global
                ) {
                    diagnostics.push(frontier_diagnostic(
                        "trace_origin_unestablished",
                        "No recorded predecessor establishes this endpoint as a value origin",
                        current_node,
                        &context,
                        &[],
                        &data_nodes,
                    ));
                }
                break;
            }
            if steps.len() >= max_depth {
                truncated = true;
                diagnostics.push(frontier_diagnostic(
                    "max_depth_truncated",
                    "Incoming edges remain beyond the requested trace depth",
                    current_node,
                    &context,
                    &candidates,
                    &data_nodes,
                ));
                break;
            }
            let (cycles, available): (Vec<_>, Vec<_>) = candidates
                .into_iter()
                .filter(|edge| edge.context.is_some())
                .partition(|edge| visited.contains(&(edge.source, edge.context.clone().unwrap())));
            if !cycles.is_empty() {
                diagnostics.push(frontier_diagnostic(
                    "trace_cycle_unexpanded",
                    "The displayed path does not unfold this cycle",
                    current_node,
                    &context,
                    &cycles,
                    &data_nodes,
                ));
            }
            let Some(best_edge) = available.last() else {
                break;
            };
            if available.len() > 1 {
                diagnostics.push(frontier_diagnostic("trace_alternatives_unexpanded", "Additional recorded edges are not expanded in this single path; they need not represent distinct runtime values", current_node, &context, &available[..available.len() - 1], &data_nodes));
            }
            let source = data_nodes[&best_edge.source].clone();
            context = best_edge.context.clone().unwrap();
            let mut step = TracePathStep::new(
                0,
                source.id,
                current_id,
                best_edge.kind,
                kind_description(&best_edge.kind),
                source.file_id,
                Some(source.range),
            );
            step.call_context = context.clone();
            steps.push(step);
            visited.insert((source.id, context.clone()));
            current = source;
        }

        let farthest_depth = steps.len();
        steps.reverse();

        // Populate evidence on every step so cross‑file virtual edges
        // carry file‑path attribution (needed by test assertions and
        // agent/AI consumers).
        for (index, step) in steps.iter_mut().enumerate() {
            step.index = index as u32;
            if step.evidence.is_none() {
                step.evidence = build_step_evidence(store, &step.file_id, &step.from_node_id);
            }
        }

        // Resolve the source node as a TracePoint
        let source_node = current;
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
            call_context: context,
            file_id: source_node.file_id,
            line: source_node.range.start_line + 1,
            column: source_node.range.start_column + 1,
            capability: sink_point.capability.clone(),
            partial_result: false,
            diagnostics: vec![],
        };

        let partial = sink_point.partial_result || !diagnostics.is_empty();

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

/// Reuse the existing diagnostic detail payload for continuation positions.
/// Bound each payload independently of the number of incoming graph edges.
fn frontier_diagnostic(
    code: &str,
    message: &str,
    at: &DataNode,
    context: &[CallsiteId],
    edges: &[&Candidate],
    nodes: &HashMap<DataNodeId, DataNode>,
) -> TraceDiagnostic {
    const MAX_DETAIL_EDGES: usize = 128;
    let position = |node: &DataNode, context: Option<&[CallsiteId]>| {
        serde_json::json!({
            "data_node_id": node.id,
            "file_id": node.file_id,
            "location": node.range,
            "line": node.range.start_line.saturating_add(1),
            "column": node.range.start_column.saturating_add(1),
            "call_context": context,
        })
    };
    let alternatives: Vec<_> = edges
        .iter()
        .take(MAX_DETAIL_EDGES)
        .map(|edge| {
            serde_json::json!({"source_id": edge.source, "target_id": edge.target,
            "edge_kind": edge.kind, "callsite_id": edge.callsite_id,
            "position": nodes.get(&edge.source).map(|node| position(node, edge.context.as_deref()))})
        })
        .collect();
    TraceDiagnostic::warning(message)
        .with_code(code)
        .with_detail(
        serde_json::json!({
            "at": position(at, Some(context)), "edges": alternatives, "total_edges": edges.len(),
            "details_truncated": edges.len() > MAX_DETAIL_EDGES,
        })
        .to_string(),
    )
}

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
            | DataFlowKind::WritebackToCall
            | DataFlowKind::ReceiverToThis
            | DataFlowKind::StateFlow
            | DataFlowKind::Phi
    )
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
        DataFlowKind::WritebackToCall => "reference exit → argument output (cross-function)",
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
    fn should_trace_phi_inputs() {
        assert!(should_trace_backward(&DataFlowKind::Phi));
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
