//! Source-position navigation using indexed callable identities. No call resolution.
use super::*;

// Body extent must end in written syntax, not a parser-inserted boundary.
// Errors inside the body do not invalidate independently recorded ownership.
// This also works for function try-blocks: inspect the outermost boundary leaves,
// rather than rejecting the entire subtree for an unrelated statement error.
fn written_body(callable: Node<'_>) -> Option<Node<'_>> {
    let body = callable.child_by_field_name("body")?;
    let first = std::iter::successors(Some(body), |n| n.child(0)).last()?;
    let last = std::iter::successors(Some(body), |n| {
        n.child(u32::try_from(n.child_count().checked_sub(1)?).ok()?)
    })
    .last()?;
    (!body.is_missing()
        && !body.is_error()
        && !first.is_missing()
        && first.start_byte() < first.end_byte()
        && !last.is_missing()
        && last.start_byte() < last.end_byte())
    .then_some(body)
}

impl Investigation<'_> {
    pub(super) fn callable_navigation(
        &mut self,
        file: &FileInfo,
        start: u32,
        end: u32,
    ) -> anyhow::Result<()> {
        let at = ContextLocation {
            file_id: file.file_id,
            range: TextRange {
                start_byte: start,
                end_byte: end,
                ..Default::default()
            },
        };
        let subject = ContextSubject::Region {
            location: at.clone(),
            symbol_id: None,
        };
        if file.language != Language::Cpp {
            return self.navigation_gap(subject, "callable_navigation_unsupported",
                "Callable navigation currently supports indexed C++; source and other recorded facts remain available.", vec![]);
        }
        let parsed = match self.source(file.file_id) {
            Ok(parsed) => parsed,
            Err(error) => {
                return self.navigation_gap(subject, "source_context_unavailable", error, vec![]);
            }
        };
        let symbols = self.store.find_symbols_by_file(&file.file_id)?;
        let trusted = |symbol: &SymbolDef| {
            !parsed
                .cpp
                .as_ref()
                .is_some_and(|facts| facts.unverified_callable_scopes.contains(&symbol.id))
        };
        let selected: Vec<_> = symbols
            .iter()
            .filter(|s| {
                matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
                && s.name_range.start_byte <= start && end <= s.name_range.end_byte
                && start < end
                // Lambda capture syntax is not a written callable name.
                && parsed.tree.root_node().descendant_for_byte_range(
                    s.range.start_byte as usize, s.range.end_byte as usize
                ).is_none_or(|n| n.kind() != "lambda_expression")
            })
            .collect();
        let mut related = vec![];
        if let [symbol] = selected.as_slice() {
            if trusted(symbol) {
                let syntax = extraction::cpp_expressions::expression_function(
                    parsed.tree.root_node(),
                    symbol.name_range,
                );
                let definition = syntax.is_some_and(|n| {
                    n.kind() == "function_definition"
                        && cpp::range(n) == symbol.range
                        && written_body(n).is_some()
                });
                self.reserve_item()?;
                self.result.items.push(CallContextItem {
                    subject: subject.clone(),
                    kind: if definition { ContextItemKind::Definition } else { ContextItemKind::Declaration },
                    role: "selected_callable",
                    location: ContextLocation { file_id: file.file_id,
                        range: if definition { symbol.range } else { symbol.name_range } },
                    symbol_id: Some(symbol.id), related_locations: vec![],
                    message: "The selection names this indexed callable declaration. A definition range is returned only when its recorded identity matches the source definition; no call target is selected.".into(),
                });
                if !definition && syntax.is_some_and(|n| cpp::range(n) == symbol.range) {
                    self.navigation_gap(subject, "callable_navigation_unavailable",
                        "The callable declaration is recorded, but the body extent depends on parser recovery. The name remains navigable; inspect the source before choosing a body range.",
                        vec![ContextLocation { file_id: file.file_id, range: symbol.range }])?;
                }
                return Ok(());
            }
        }
        for symbol in selected {
            self.check()?;
            related.push(ContextLocation {
                file_id: file.file_id,
                range: symbol.name_range,
            });
        }
        let syntax =
            extraction::cpp_expressions::expression_function(parsed.tree.root_node(), at.range);
        if let Some(syntax) = syntax
            && written_body(syntax).is_some_and(|body| {
                body.start_byte() <= start as usize && end as usize <= body.end_byte()
            })
            && !self.crosses_nested_body(syntax, start, end)?
            && let Some(owner) = self.function_owner(&at, &parsed)?
        {
            self.reserve_item()?;
            self.result.items.push(CallContextItem {
                subject, kind: ContextItemKind::Definition, role: "enclosing_callable",
                location: ContextLocation { file_id: file.file_id, range: owner.range },
                symbol_id: Some(owner.id), related_locations: vec![],
                message: "This indexed function body owns the selected source position. Containment does not identify a selected call's target or prove execution.".into(),
            });
            return Ok(());
        }
        // Related recorded ranges remain useful without choosing an owner.
        for symbol in symbols.iter().filter(|s| {
            matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
                && s.range.start_byte < end
                && start < s.range.end_byte
        }) {
            self.check()?;
            let location = ContextLocation {
                file_id: file.file_id,
                range: symbol.range,
            };
            if !related.contains(&location) {
                related.push(location);
            }
        }
        self.navigation_gap(subject, "callable_navigation_unavailable",
            "No supported indexed callable name or single function body owns this selection. Inspect the source and related ranges; signature regions, ambiguous or unverified identities and selections spanning bodies do not establish an owner.", related)
    }

    fn crosses_nested_body(&self, owner: Node<'_>, start: u32, end: u32) -> anyhow::Result<bool> {
        let mut cursor = owner.walk();
        loop {
            self.check()?;
            let node = cursor.node();
            let overlaps = node.start_byte() < end as usize && (start as usize) < node.end_byte();
            if overlaps
                && node != owner
                && matches!(node.kind(), "function_definition" | "lambda_expression")
                && node.child_by_field_name("body").is_some_and(|body| {
                    body.start_byte() < end as usize && (start as usize) < body.end_byte()
                })
            {
                return Ok(true);
            }
            if overlaps && cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return Ok(false);
                }
            }
        }
    }

    fn navigation_gap(
        &mut self,
        subject: ContextSubject,
        code: &'static str,
        message: impl Into<String>,
        related_locations: Vec<ContextLocation>,
    ) -> anyhow::Result<()> {
        self.reserve_item()?;
        self.result.gaps.push(CallContextGap {
            subject,
            code,
            message: message.into(),
            related_locations,
        });
        Ok(())
    }
}
