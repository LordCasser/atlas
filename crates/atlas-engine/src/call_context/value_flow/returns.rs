//! Required boolean checks constrain literal exits of the same invocation.
//! Unknown expressions remain alternatives; only a direct boolean-return
//! wrapper forwards the constraint to another recorded call.
use super::*;
use tree_sitter::Node;

#[derive(Clone)]
struct Truth {
    value: bool,
    evidence: Vec<ContextLocation>,
}

#[derive(Default)]
pub(super) struct ReturnConditions {
    checks: Option<Vec<(ContextLocation, bool)>>,
    expected: BTreeMap<Vec<CallsiteId>, Option<Truth>>,
    boolean_functions: BTreeMap<SymbolId, bool>,
}

fn child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    let mut children = node
        .named_children(&mut cursor)
        .filter(|n| n.kind() != "comment");
    let first = children.next()?;
    children.next().is_none().then_some(first)
}

/// The expression must be exactly a call, optionally parenthesized/negated.
/// Its result type is independently required to be builtin bool.
fn direct_call<'tree>(mut node: Node<'tree>, source: &str) -> Option<(Node<'tree>, bool)> {
    let mut inverted = false;
    loop {
        if node.has_error() || node.is_missing() {
            return None;
        }
        match node.kind() {
            "condition_clause" | "parenthesized_expression" | "return_statement" => {
                node = child(node)?
            }
            "unary_expression"
                if node
                    .child_by_field_name("operator")?
                    .utf8_text(source.as_bytes())
                    .ok()?
                    == "!" =>
            {
                inverted = !inverted;
                node = node.child_by_field_name("argument")?;
            }
            "call_expression" => return Some((node, inverted)),
            _ => return None,
        }
    }
}

fn literal_return(node: Node<'_>) -> Option<bool> {
    if node.kind() != "return_statement" || node.has_error() {
        return None;
    }
    let mut value = child(node)?;
    while value.kind() == "parenthesized_expression" {
        value = child(value)?;
    }
    match value.kind() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

impl Query<'_> {
    fn boolean_function(&mut self, owner: &SymbolDef) -> anyhow::Result<bool> {
        if let Some(value) = self.return_conditions.boolean_functions.get(&owner.id) {
            return Ok(*value);
        }
        let value = if let Some(parsed) = self.source(&location(owner.file_id, owner.name_range))? {
            cpp::callable(parsed.tree.root_node(), owner.name_range)
                .and_then(|c| c.type_node)
                .is_some_and(|ty| {
                    ty.kind() == "primitive_type"
                        && ty.utf8_text(parsed.source.as_bytes()).ok() == Some("bool")
                        && ty
                            .parent()
                            .and_then(|p| p.child_by_field_name("declarator"))
                            .is_some_and(|d| d.kind() == "function_declarator" && !d.has_error())
                })
        } else {
            false
        };
        self.return_conditions
            .boolean_functions
            .insert(owner.id, value);
        Ok(value)
    }

    fn root_return_checks(&mut self, at: &ContextLocation) -> anyhow::Result<()> {
        if self.return_conditions.checks.is_some() {
            return Ok(());
        }
        let first_item = self.investigation.result.items.len();
        let first_gap = self.investigation.result.gaps.len();
        self.investigation.control_conditions(
            at.file_id,
            at.range.start_byte,
            at.range.end_byte,
        )?;
        let checks = self.investigation.result.items[first_item..]
            .iter()
            .filter_map(|item| {
                let value = match item.role {
                    "control_true_branch" => true,
                    "control_false_branch" => false,
                    _ => return None,
                };
                Some((item.location.clone(), value))
            })
            .collect();
        for gap in &self.investigation.result.gaps[first_gap..] {
            self.result.gaps.push((
                gap.subject.location(),
                gap.code.into(),
                gap.message.clone(),
                gap.related_locations.clone(),
            ));
            self.result.truncated |=
                gap.code == "control_analysis_budget" || gap.code == "control_analysis_incomplete";
        }
        self.return_conditions.checks = Some(checks);
        Ok(())
    }

    fn expected_return(
        &mut self,
        invocation: &[CallsiteId],
        requested: &ContextLocation,
        root_context: &[CallsiteId],
    ) -> anyhow::Result<Option<Truth>> {
        self.investigation.check()?;
        if invocation.len() <= root_context.len() || !invocation.starts_with(root_context) {
            return Ok(None);
        }
        if let Some(cached) = self.return_conditions.expected.get(invocation) {
            return Ok(cached.clone());
        }
        let calls = self
            .temporary
            .find_resolved_callsites_by_id(invocation.last().unwrap())?;
        let [call] = calls.as_slice() else {
            return Ok(None);
        };
        let Some(callee) = self.temporary.find_symbol_by_id(&call.callee)? else {
            return Ok(None);
        };
        if !self.boolean_function(&callee)? {
            self.return_conditions
                .expected
                .insert(invocation.to_vec(), None);
            return Ok(None);
        }
        let Some(caller) = self.temporary.find_symbol_by_id(&call.callsite.caller)? else {
            return Ok(None);
        };
        let Some(parsed) = self.source(&location(caller.file_id, caller.range))? else {
            return Ok(None);
        };
        let caller_context = &invocation[..invocation.len() - 1];
        let mut expectations = Vec::new();
        if caller_context == root_context {
            self.root_return_checks(requested)?;
            for (condition, expected) in self.return_conditions.checks.as_ref().unwrap() {
                if condition.file_id != caller.file_id {
                    continue;
                }
                let syntax = cpp::expression(parsed.tree.root_node(), condition.range);
                if let Some((expression, inverted)) =
                    syntax.and_then(|n| direct_call(n, &parsed.source))
                    && cpp::range(expression) == call.callsite.range
                {
                    expectations.push(Truth {
                        value: *expected != inverted,
                        evidence: vec![
                            condition.clone(),
                            location(caller.file_id, call.callsite.range),
                        ],
                    });
                }
            }
        } else if self.boolean_function(&caller)? {
            // A whole body consisting of one return directly forwards this
            // call's boolean result on every supported normal exit. A selected
            // path through a more complex wrapper is insufficient evidence.
            let returned = cpp::callable(parsed.tree.root_node(), caller.name_range)
                .and_then(|c| c.type_node)
                .and_then(|ty| ty.parent())
                .and_then(|function| function.child_by_field_name("body"))
                .and_then(child)
                .filter(|statement| statement.kind() == "return_statement");
            if let Some(statement) = returned
                && let Some((expression, inverted)) = direct_call(statement, &parsed.source)
                && cpp::range(expression) == call.callsite.range
                && let Some(mut expected) =
                    self.expected_return(caller_context, requested, root_context)?
            {
                expected.value ^= inverted;
                expected
                    .evidence
                    .push(location(caller.file_id, cpp::range(statement)));
                expectations.push(expected);
            }
        }
        let answer = expectations
            .first()
            .cloned()
            .filter(|first| expectations.iter().all(|t| t.value == first.value));
        self.return_conditions
            .expected
            .insert(invocation.to_vec(), answer.clone());
        Ok(answer)
    }

    pub(super) fn constrain_return_exits(
        &mut self,
        trace: &TracePath,
        requested: &ContextLocation,
        root_context: &[CallsiteId],
        engine: &mut analysis::trace::TraceEngine,
    ) -> anyhow::Result<bool> {
        let mut changed = false;
        for step in &trace.steps {
            if step.edge_kind != DataFlowKind::WritebackToCall {
                continue;
            }
            let Some(exit) = self.temporary.get_data_node(&step.from_node_id)? else {
                continue;
            };
            if exit.kind != DataNodeKind::ParameterOutput {
                continue;
            }
            let Some(parsed) = self.source(&location(exit.file_id, exit.range))? else {
                continue;
            };
            let Some(function) = exit.function_id else {
                continue;
            };
            let alternatives: Vec<_> = self
                .temporary
                .find_data_nodes_by_function(&function)?
                .into_iter()
                .filter(|n| {
                    n.kind == DataNodeKind::ParameterOutput
                        && n.arg_index == exit.arg_index
                        && n.binding_id == exit.binding_id
                })
                .filter_map(|n| {
                    cpp::expression(parsed.tree.root_node(), n.range)
                        .and_then(literal_return)
                        .map(|value| (n, value))
                })
                .collect();
            if alternatives.is_empty() {
                continue;
            }
            let Some(expected) =
                self.expected_return(&step.call_context, requested, root_context)?
            else {
                continue;
            };
            for (exit, actual) in alternatives {
                self.investigation.check()?;
                if expected.value == actual {
                    continue;
                }
                if engine.exclude_return(exit.id, step.call_context.clone()) {
                    changed = true;
                    let mut related = expected.evidence.clone();
                    related.extend(self.point(&exit, &step.call_context)?.call_context);
                    self.result.gaps.push((location(exit.file_id, exit.range), "value_return_condition_excluded".into(),
                        format!("This recorded exit returns {actual}, conflicting with the required boolean result {} for this invocation. It is excluded only under the located caller condition; the exit facts, unknown alternatives and overall execution limits remain.", expected.value), related));
                }
            }
        }
        Ok(changed)
    }
}
