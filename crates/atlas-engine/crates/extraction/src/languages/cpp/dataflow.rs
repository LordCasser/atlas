//! A member access consumes its written receiver expression. A previous
//! same-name declaration is not a substitute for that expression's value.
use std::collections::{HashMap, HashSet};

use types::{DataFlowEdge, DataFlowEdgeId, DataFlowKind, DataNode, DataNodeId, DataNodeKind};

use crate::{dataflow_builder::NodePosKey, extraction_ctx::ExtractionCtx};

/// Correct only data nodes inside recorded capture lists, before constructing
/// function-local edges. Body nodes and other language ownership stay intact.
pub(crate) fn capture_initializer_owners(
    ctx: &ExtractionCtx<'_>,
    symbols: &[types::SymbolDef],
    nodes: &mut [DataNode],
) {
    let initializers = super::lambdas::caller_initializers(ctx.root, symbols);
    for node in nodes {
        let Some(range) = node.function_id.and_then(|id| initializers.get(&id)) else {
            continue;
        };
        if range.start_byte > node.range.start_byte || node.range.end_byte > range.end_byte {
            continue;
        }
        let owner = crate::cpp_expressions::expression_function(ctx.root, node.range);
        node.function_id = owner.and_then(|owner| {
            let mut matched = symbols.iter().filter(|symbol| {
                matches!(
                    symbol.kind,
                    types::SymbolKind::Function
                        | types::SymbolKind::Method
                        | types::SymbolKind::Constructor
                ) && symbol.range.start_byte == owner.start_byte() as u32
                    && symbol.range.end_byte == owner.end_byte() as u32
            });
            let selected = matched.next()?;
            matched.next().is_none().then_some(selected.id)
        });
    }
}

pub(super) fn field_receivers(
    ctx: &ExtractionCtx<'_>,
    positions: &HashMap<NodePosKey, DataNodeId>,
    nodes: &[DataNode],
    edges: &mut Vec<DataFlowEdge>,
) {
    let fields: HashSet<_> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Field)
        .map(|n| n.id)
        .collect();
    // Replace the shared name-based projection; do not keep it as another
    // possible receiver when the exact use is available or was not extracted.
    edges.retain(|e| e.kind != DataFlowKind::FieldLoad || !fields.contains(&e.target));
    let by_id: HashMap<_, _> = nodes.iter().map(|n| (n.id, n)).collect();
    for field in nodes.iter().filter(|n| n.kind == DataNodeKind::Field) {
        let Some(node) = ctx.root.descendant_for_byte_range(
            field.range.start_byte as usize,
            field.range.end_byte as usize,
        ) else {
            continue;
        };
        let Some(access) = std::iter::successors(Some(node), |n| n.parent()).find(|n| {
            n.kind() == "field_expression"
                && (n.start_byte() == field.range.start_byte as usize
                    && n.end_byte() == field.range.end_byte as usize
                    || n.child_by_field_name("field").is_some_and(|name| {
                        name.start_byte() == field.range.start_byte as usize
                            && name.end_byte() == field.range.end_byte as usize
                    }))
        }) else {
            continue;
        };
        let Some(receiver) = access.child_by_field_name("argument") else {
            continue;
        };
        let Some(id) = positions.get(&NodePosKey {
            start_byte: receiver.start_byte() as u32,
            end_byte: receiver.end_byte() as u32,
            kind: DataNodeKind::Receiver,
        }) else {
            continue;
        };
        if by_id
            .get(id)
            .is_none_or(|n| n.function_id != field.function_id)
        {
            continue;
        }
        edges.push(DataFlowEdge::new(
            DataFlowEdgeId::generate(id, &field.id, DataFlowKind::FieldLoad.as_str()),
            *id,
            field.id,
            DataFlowKind::FieldLoad,
            field.range,
            0.80,
        ));
    }
}
