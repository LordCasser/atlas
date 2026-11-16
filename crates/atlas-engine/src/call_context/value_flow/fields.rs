//! A field's receiver locates an object; it does not supply the stored value.
use super::*;

impl Query<'_> {
    pub(super) fn limit_field_reads(
        &mut self,
        facts: &mut FileFacts,
        parsed: &ParsedSource,
    ) -> anyhow::Result<()> {
        let stored: BTreeSet<_> = facts
            .dataflow_edges
            .iter()
            .filter(|edge| edge.kind == DataFlowKind::FieldStore)
            .map(|edge| edge.target)
            .collect();
        let nodes: BTreeMap<_, _> = facts
            .data_nodes
            .iter()
            .map(|node| (node.id, node))
            .collect();
        for edge in &facts.dataflow_edges {
            self.investigation.check()?;
            if edge.kind != DataFlowKind::FieldLoad
                || stored.contains(&edge.target)
                || !nodes
                    .get(&edge.target)
                    .is_some_and(|node| node.kind == DataNodeKind::Field)
            {
                continue;
            }
            // A directly addressed member uses its receiver as object/address
            // context. Preserve those existing dependencies and their limits;
            // obtaining a member address does not require its stored contents.
            let mut syntax = cpp::expression(parsed.tree.root_node(), nodes[&edge.target].range);
            let mut addressed = false;
            while let Some(node) = syntax {
                if node.kind() == "pointer_expression"
                    && node
                        .child_by_field_name("operator")
                        .is_some_and(|op| cpp::text(op, &parsed.source) == "&")
                {
                    addressed = true;
                    break;
                }
                if !matches!(
                    node.kind(),
                    "field_identifier"
                        | "identifier"
                        | "field_expression"
                        | "parenthesized_expression"
                ) {
                    break;
                }
                syntax = node.parent();
            }
            if addressed {
                continue;
            }
            let receivers = self.field_receivers.entry(edge.target).or_default();
            if let Some(receiver) = nodes.get(&edge.source) {
                receivers.push(location(receiver.file_id, receiver.range));
            }
        }
        // Only remove receiver projections in this disposable value graph.
        // FieldStore still traces the assigned input; no store/read or object
        // identity is inferred. The Ready graph and receiver locations remain.
        facts.dataflow_edges.retain(|edge| {
            edge.kind != DataFlowKind::FieldLoad || !self.field_receivers.contains_key(&edge.target)
        });
        Ok(())
    }
}
