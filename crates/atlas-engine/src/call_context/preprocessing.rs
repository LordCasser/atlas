//! Written enclosing conditional directives. They locate configuration questions;
//! they do not evaluate macros or establish which branch is active.
use super::*;

impl Investigation<'_> {
    pub(super) fn preprocessing_context(
        &mut self,
        file_id: FileId,
        start: u32,
        end: u32,
    ) -> anyhow::Result<()> {
        let subject = ContextSubject::Region {
            location: ContextLocation {
                file_id,
                range: TextRange {
                    start_byte: start,
                    end_byte: end,
                    ..Default::default()
                },
            },
            symbol_id: None,
        };
        let parsed = match self.source(file_id) {
            Ok(parsed) => parsed,
            Err(message) => {
                self.reserve_item()?;
                self.result.gaps.push(CallContextGap {
                    subject,
                    code: "preprocessing_context_unavailable",
                    message,
                    related_locations: vec![],
                });
                return Ok(());
            }
        };
        let Some(selected) = parsed
            .tree
            .root_node()
            .descendant_for_byte_range(start as usize, end as usize)
        else {
            return Ok(());
        };
        // Recovery before/around this selection can omit or misnest an opening
        // directive. Preserve the concrete unparsed regions and the independent
        // text-search continuation instead of treating AST ancestry as complete.
        let mut unexamined = vec![];
        let mut pending = vec![parsed.tree.root_node()];
        while let Some(node) = pending.pop() {
            self.check()?;
            if node.start_byte() >= end as usize {
                continue;
            }
            if node.is_error() || node.is_missing() {
                unexamined.push(loc(file_id, node));
            } else if node.has_error() {
                let mut cursor = node.walk();
                pending.extend(node.children(&mut cursor));
            }
        }
        if !unexamined.is_empty() {
            unexamined.sort_by_key(|at| at.range.start_byte);
            self.reserve_item()?;
            self.result.gaps.push(CallContextGap {
                subject: subject.clone(),
                code: "preprocessing_context_incomplete",
                message: "Syntax recovery before or around this selection may omit or misnest conditional directives. The returned ancestry is not a complete condition inventory. Use text-search on this source file to find remaining directive text; those matches can include comments/literals and do not select an active configuration.".into(),
                related_locations: unexamined,
            });
        }
        let mut directives = vec![];
        for node in std::iter::successors(Some(selected), |node| node.parent()) {
            self.check()?;
            if !matches!(
                node.kind(),
                "preproc_if"
                    | "preproc_ifdef"
                    | "preproc_elif"
                    | "preproc_elifdef"
                    | "preproc_else"
            ) {
                continue;
            }
            let tail = node
                .child_by_field_name("condition")
                .or_else(|| node.child_by_field_name("name"))
                .or_else(|| {
                    (node.kind() == "preproc_else")
                        .then(|| node.child(0))
                        .flatten()
                });
            let Some(tail) = tail.filter(|tail| !tail.is_missing() && !tail.has_error()) else {
                self.reserve_item()?;
                self.result.gaps.push(CallContextGap {
                    subject: subject.clone(),
                    code: "preprocessing_syntax_unexamined",
                    message: "The enclosing directive has no complete supported condition syntax. Read the directive and surrounding source; no configuration condition is established.".into(),
                    related_locations: vec![loc(file_id, node.child(0).unwrap_or(node))],
                });
                continue;
            };
            // A selected directive's own condition is not its controlled body.
            // For an alternative, the ancestor chain also retains the previous
            // if/elif headers; those preceding tests are not all true guards.
            if start < tail.end_byte() as u32 {
                continue;
            }
            if node
                .child_by_field_name("alternative")
                .is_some_and(|alternative| {
                    start < alternative.start_byte() as u32 && end > alternative.start_byte() as u32
                })
            {
                continue; // The selection straddles branches of this group.
            }
            let mut range = cpp::range(node);
            let tail = cpp::range(tail);
            range.end_byte = tail.end_byte;
            range.end_line = tail.end_line;
            range.end_column = tail.end_column;
            directives.push(ContextLocation { file_id, range });
        }
        directives.sort_by_key(|at| at.range.start_byte);
        for location in &directives {
            self.reserve_item()?;
            self.result.items.push(CallContextItem {
                kind: ContextItemKind::Reference,
                subject: subject.clone(),
                role: "preprocessing_directive",
                location: location.clone(),
                symbol_id: None,
                related_locations: vec![],
                message: "Written directive on the syntax path enclosing the selection. Earlier if/elif tests and the selected alternative remain separate source clues; these are not runtime guards or evaluated configuration facts.".into(),
            });
        }
        if !directives.is_empty() {
            self.reserve_item()?;
            self.result.gaps.push(CallContextGap {
                subject,
                code: "preprocessing_configuration_unestablished",
                message: "Enclosing conditional-compilation directives are available for source investigation. This query has not evaluated macro definitions, include tests or their order, and has not selected a build variant. Nested directives inside a wider selection and unparsed directives are not exhaustively enumerated.".into(),
                related_locations: directives,
            });
        }
        Ok(())
    }
}
