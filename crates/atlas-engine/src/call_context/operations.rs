//! Lazy source regions outside ordinary call-reference invocation analysis.
//! Operand calls and value dependencies retain their independent facts.
use super::*;

impl Investigation<'_> {
    pub(super) fn operation_regions(
        &mut self,
        file_id: FileId,
        start: u32,
        end: u32,
    ) -> anyhow::Result<()> {
        let parsed = match self.source(file_id) {
            Ok(source) => source,
            // Existing source/parse diagnostics retain the failed read. Do not
            // invent expression boundaries without its tree.
            Err(_) => return Ok(()),
        };
        let Some(root) = parsed
            .tree
            .root_node()
            .descendant_for_byte_range(start as usize, end as usize)
        else {
            return Ok(());
        };
        let symbols = self.store.find_symbols_by_file(&file_id)?;
        let mut owners: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for symbol in symbols {
            self.check()?;
            if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method)
                && !parsed
                    .cpp
                    .as_ref()
                    .is_some_and(|f| f.unverified_callable_scopes.contains(&symbol.id))
            {
                owners
                    .entry((symbol.range.start_byte, symbol.range.end_byte))
                    .or_default()
                    .push(symbol.id);
            }
        }
        let mut pending = vec![root];
        while let Some(node) = pending.pop() {
            self.check()?;
            if node.end_byte() <= start as usize || end as usize <= node.start_byte() {
                continue;
            }
            if let Some(range) =
                extraction::cpp_expressions::unexamined_invocation_region(node, &parsed.source)
                && start <= range.start_byte
                && range.end_byte <= end
            {
                let owner = std::iter::successors(node.parent(), |parent| parent.parent())
                    .find(|parent| {
                        matches!(parent.kind(), "function_definition" | "lambda_expression")
                            && parent.child_by_field_name("body").is_some_and(|body| {
                                body.start_byte() <= node.start_byte()
                                    && node.end_byte() <= body.end_byte()
                            })
                    })
                    .and_then(|parent| {
                        owners.get(&(parent.start_byte() as u32, parent.end_byte() as u32))
                    })
                    .and_then(|ids| match ids.as_slice() {
                        [id] => Some(*id),
                        _ => None,
                    });
                self.reserve_item()?;
                self.result.gaps.push(CallContextGap {
                    subject: ContextSubject::Region { location: ContextLocation { file_id, range }, symbol_id: owner },
                    code: "cpp_operation_invocation_unexamined",
                    message: "This syntax operation was located, but its operator, conversion or implicit invocation semantics have not been analyzed by this query. It may use built-in operations only. Calls in its operands remain independent facts. Read this region before treating the body as covered; this is not proof of a call, a selected target, evaluation or complete body coverage.".into(),
                    related_locations: vec![],
                });
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        Ok(())
    }
}
