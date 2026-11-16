//! Separate an invocation's result from evaluation of its receiver/arguments.
//!
//! This consumes syntax result captures and recorded Callsite ranges. Syntax
//! alone establishes a boundary, not a target or a callee-return mapping.
//! It does not infer targets, return types, effects or runtime dispatch.

use std::collections::{HashMap, HashSet};

use types::{
    Callsite, CallsiteId, DataFlowEdge, DataFlowEdgeId, DataFlowKind, DataNode, DataNodeId,
    DataNodeKind, SymbolId, TextRange,
};

use crate::{CancelCheck, extraction_ctx::ExtractionCtx};

fn contains(outer: TextRange, inner: TextRange) -> bool {
    outer.start_byte <= inner.start_byte && inner.end_byte <= outer.end_byte
}

/// Return None on cancellation; a canceled file does not publish partial facts.
pub(crate) fn connect(
    ctx: &ExtractionCtx<'_>,
    callsites: &[Callsite],
    value_calls: &HashSet<CallsiteId>,
    nodes: &mut Vec<DataNode>,
    edges: &mut Vec<DataFlowEdge>,
    capture_ranges: Option<&[(u32, u32)]>,
    cancel: &dyn CancelCheck,
) -> Option<()> {
    let mut by_function: HashMap<Option<SymbolId>, Vec<DataNode>> = HashMap::new();
    let mut recorded_ranges = HashSet::new();
    for call in callsites {
        if cancel.is_cancelled() {
            return None;
        }
        if !value_calls.contains(&call.id)
            || capture_ranges.is_some_and(|ranges| {
                !ranges.iter().any(|&(start, end)| {
                    start <= call.range.start_byte && call.range.end_byte <= end
                })
            })
        {
            continue;
        }
        // The recorded invocation identity distinguishes nested calls sharing
        // a start position, such as factory()(). No name-based target lookup.
        let id = DataNodeId::generate(
            &ctx.file_id,
            Some(&call.caller),
            "call_return",
            Some(&call.id.to_string()),
            None,
            call.range.start_byte,
        );
        let result = DataNode {
            id,
            file_id: ctx.file_id,
            function_id: Some(call.caller),
            kind: DataNodeKind::CallReturn,
            binding_id: None,
            callsite_id: Some(call.id),
            name: ctx
                .source
                .get(call.range.start_byte as usize..call.range.end_byte as usize)
                .map(str::to_owned),
            access_path: None,
            arg_index: None,
            range: call.range,
        };
        recorded_ranges.insert((
            Some(call.caller),
            call.range.start_byte,
            call.range.end_byte,
        ));
        by_function
            .entry(Some(call.caller))
            .or_default()
            .push(result);
    }
    // Full and Lazy share the dataflow captures and callable ownership. Keep
    // syntax-only results opaque, including construction: constructor return
    // statements do not supply the value of the created object/pointer.
    // No reference extraction, name lookup or provisional callsite join here.
    let mut operands = Vec::with_capacity(nodes.len());
    for node in nodes.drain(..) {
        if cancel.is_cancelled() {
            return None;
        }
        if node.kind == DataNodeKind::CallReturn {
            if !recorded_ranges.contains(&(
                node.function_id,
                node.range.start_byte,
                node.range.end_byte,
            )) {
                by_function.entry(node.function_id).or_default().push(node);
            }
        } else {
            operands.push(node);
        }
    }
    *nodes = operands;
    let by_id: HashMap<_, _> = nodes.iter().map(|n| (n.id, n)).collect();
    let mut retained = Vec::with_capacity(edges.len());
    for edge in edges.drain(..) {
        if cancel.is_cancelled() {
            return None;
        }
        let crosses_call = matches!(edge.kind, DataFlowKind::Read | DataFlowKind::ReturnValue)
            && by_id
                .get(&edge.source)
                .zip(by_id.get(&edge.target))
                .is_some_and(|(source, target)| {
                    source.function_id == target.function_id
                        && by_function.get(&source.function_id).is_some_and(|calls| {
                            calls.iter().any(|call| {
                                contains(call.range, source.range)
                                    && contains(target.range, call.range)
                            })
                        })
                });
        if !crosses_call {
            retained.push(edge);
        }
    }
    *edges = retained;
    let mut results = Vec::new();
    for (function, calls) in &by_function {
        for result in calls {
            for consumer in nodes.iter().filter(|node| {
                node.function_id == *function
                    && matches!(
                        node.kind,
                        DataNodeKind::Expr
                            | DataNodeKind::Return
                            | DataNodeKind::CallArg
                            | DataNodeKind::Receiver
                    )
                    && contains(node.range, result.range)
            }) {
                if cancel.is_cancelled() {
                    return None;
                }
                // g(f(x), y) consumes g's result. f's result belongs to the
                // corresponding argument of g, not directly to g's consumer.
                if calls.iter().any(|outer| {
                    contains(outer.range, result.range)
                        && (outer.range.start_byte != result.range.start_byte
                            || outer.range.end_byte != result.range.end_byte)
                        && contains(consumer.range, outer.range)
                }) {
                    continue;
                }
                let kind = if consumer.kind == DataNodeKind::Return {
                    DataFlowKind::ReturnValue
                } else {
                    DataFlowKind::Read
                };
                edges.push(DataFlowEdge::new(
                    DataFlowEdgeId::generate(&result.id, &consumer.id, kind.as_str()),
                    result.id,
                    consumer.id,
                    kind,
                    consumer.range,
                    0.85,
                ));
            }
            results.push(result.clone());
        }
    }
    // HashMap iteration must not change persisted node/edge order or which
    // records survive an existing lazy-window budget.
    results.sort_by_key(|node| (node.range.start_byte, node.range.end_byte, node.id));
    nodes.extend(results);
    edges.sort_by_key(|edge| edge.id);
    Some(())
}
