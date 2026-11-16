//! Declaration navigation for source regions without stored calls. This reads
//! identifier syntax directly: a structural Artifact need not contain BindingUses.
use super::*;

impl Investigation<'_> {
    fn region_gap(
        &mut self,
        location: ContextLocation,
        code: &'static str,
        message: impl Into<String>,
    ) -> anyhow::Result<()> {
        self.reserve_item()?;
        self.result.gaps.push(CallContextGap {
            subject: ContextSubject::Region {
                location,
                symbol_id: None,
            },
            code,
            message: message.into(),
            related_locations: vec![],
        });
        Ok(())
    }

    pub(super) fn values(
        &mut self,
        file: &FileInfo,
        start: u32,
        end: u32,
        references: &[ReferenceUse],
        bindings: &[BindingDef],
        scopes: &BTreeMap<ScopeId, ScopeDef>,
    ) -> anyhow::Result<()> {
        let requested = ContextLocation {
            file_id: file.file_id,
            range: TextRange {
                start_byte: start,
                end_byte: end,
                ..Default::default()
            },
        };
        let parsed = match self.source(file.file_id) {
            Ok(source) => source,
            Err(error) => return self.region_gap(requested, "source_context_unavailable", error),
        };
        let symbols = self.store.find_symbols_by_file(&file.file_id)?;
        let declared: BTreeSet<_> = bindings
            .iter()
            .map(|b| (b.range.start_byte, b.range.end_byte))
            .chain(
                symbols
                    .iter()
                    .map(|s| (s.name_range.start_byte, s.name_range.end_byte)),
            )
            .collect();
        let owner = |node: Node<'_>| {
            let function = std::iter::successors(Some(node), |n| n.parent())
                .find(|n| matches!(n.kind(), "function_definition" | "lambda_expression"))?;
            let body = function.child_by_field_name("body")?;
            if node.start_byte() < body.start_byte() || node.end_byte() > body.end_byte() {
                return None;
            }
            let matched: Vec<_> = symbols
                .iter()
                .filter(|s| {
                    matches!(s.kind, SymbolKind::Function | SymbolKind::Method)
                        && s.range == cpp::range(function)
                        && !parsed
                            .cpp
                            .as_ref()
                            .is_some_and(|facts| facts.unverified_callable_scopes.contains(&s.id))
                })
                .collect();
            match matched.as_slice() {
                [symbol] => Some(symbol.id),
                _ => None,
            }
        };
        let mut receivers = vec![];
        for field in references.iter().filter(|r| {
            r.kind == ReferenceKind::FieldAccess
                && r.range.start_byte < end
                && start < r.range.end_byte
        }) {
            self.check()?;
            let mut field = field.clone();
            field.source_symbol =
                cpp::expression(parsed.tree.root_node(), field.range).and_then(owner);
            if let Some(receiver) = cpp::receiver(parsed.tree.root_node(), field.range) {
                receivers.push(cpp::range(receiver));
            }
            self.receiver(&field, references, bindings, scopes)?;
        }
        let mut cursor = parsed.tree.root_node().walk();
        loop {
            self.check()?;
            let node = cursor.node();
            let at = cpp::range(node);
            let overlaps = at.start_byte < end && start < at.end_byte
                || at.start_byte == at.end_byte && start <= at.start_byte && at.start_byte <= end;
            if overlaps && (node.is_error() || node.is_missing()) {
                self.region_gap(loc(file.file_id, node), "value_syntax_unexamined",
                    "Parser recovery may omit value uses in this region; source/text search remains available.")?;
            }
            if overlaps
                && node.kind() == "identifier"
                && !declared.contains(&(at.start_byte, at.end_byte))
                && !receivers
                    .iter()
                    .any(|r| r.start_byte <= at.start_byte && at.end_byte <= r.end_byte)
                && !node.parent().is_some_and(|n| {
                    matches!(
                        n.kind(),
                        "qualified_identifier" | "goto_statement" | "labeled_statement"
                    )
                })
            {
                let scope = scopes
                    .values()
                    .filter(|s| {
                        s.range.start_byte <= at.start_byte && at.end_byte <= s.range.end_byte
                    })
                    .min_by_key(|s| s.range.byte_len())
                    .map(|s| s.id);
                let name = cpp::text(node, &parsed.source);
                let source_symbol = owner(node);
                // A temporary usage observation supplies existing lexical/capture
                // lookup with a location. It is emitted only as a Region: its
                // internal ID is never represented as a stored reference/callsite.
                let reference = ReferenceUse {
                    id: ReferenceId::generate(
                        &file.file_id,
                        source_symbol.as_ref(),
                        at.start_byte,
                        at.end_byte,
                        name,
                        ReferenceKind::Usage,
                    ),
                    file_id: file.file_id,
                    source_symbol,
                    scope_id: scope,
                    kind: ReferenceKind::Usage,
                    text: name.into(),
                    name: name.into(),
                    receiver: Some(name.into()),
                    arity: None,
                    range: at,
                    binding_id: None,
                    resolved: None,
                };
                let syntax_unknown = std::iter::successors(Some(node), |n| n.parent())
                    .take_while(|n| {
                        !matches!(
                            n.kind(),
                            "compound_statement" | "lambda_expression" | "function_definition"
                        )
                    })
                    .any(|n| {
                        n.is_error()
                            || n.is_missing()
                            || matches!(
                                n.kind(),
                                "function_declarator"
                                    | "array_declarator"
                                    | "structured_binding_declarator"
                            )
                    });
                if syntax_unknown {
                    self.gap(&reference, "value_syntax_unexamined", "Declaration or recovered syntax does not establish a value use here; inspect the written region.", vec![])?;
                } else {
                    self.receiver(&reference, references, bindings, scopes)?;
                    if parsed.cpp.as_ref().is_some_and(|facts| {
                        facts
                            .lookup_limits
                            .iter()
                            .any(|limit| limit.limits_local_lookup(name, at.start_byte))
                    }) {
                        self.gap(&reference, "value_lookup_unverified", "Recorded declarations restrict lookup at this use. Declaration locations remain candidates, not a selected value or field identity.", vec![])?;
                    }
                }
            }
            if overlaps && cursor.goto_first_child() {
                continue;
            }
            while !cursor.goto_next_sibling() {
                if !cursor.goto_parent() {
                    return self.region_gap(requested, "value_context_limited",
                        "Examined written identifiers and stored field accesses in this region. Declaration/type/initializer locations do not establish reaching values, field aliasing, store-to-read correspondence or complete use coverage; source/text search remains independently available.");
                }
            }
        }
    }
}
