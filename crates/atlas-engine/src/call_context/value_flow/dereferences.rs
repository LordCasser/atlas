//! The operand locates a dereferenced object; it is not the produced value.
//! Keep that address expression available for a separate source/value query.
use super::*;

impl Query<'_> {
    pub(super) fn limit_dereferences(
        &mut self,
        facts: &mut FileFacts,
        parsed: &ParsedSource,
    ) -> anyhow::Result<()> {
        for node in &facts.data_nodes {
            self.investigation.check()?;
            if !matches!(node.kind, DataNodeKind::Expr | DataNodeKind::CallArg) {
                continue;
            }
            let Some(syntax) = cpp::expression(parsed.tree.root_node(), node.range) else {
                continue;
            };
            if syntax.kind() != "pointer_expression"
                || syntax.has_error()
                || syntax
                    .child_by_field_name("operator")
                    .is_none_or(|op| op.utf8_text(parsed.source.as_bytes()).ok() != Some("*"))
            {
                continue;
            }
            if let Some(operand) = syntax.child_by_field_name("argument") {
                self.dereference_inputs
                    .insert(node.id, location(node.file_id, cpp::range(operand)));
            }
        }
        // This disposable value graph must not follow address dependencies as
        // loaded values or expand address-returning callees to explain a load.
        // The immutable graph and exact operand source remain available.
        facts
            .dataflow_edges
            .retain(|edge| !self.dereference_inputs.contains_key(&edge.target));
        Ok(())
    }
}
