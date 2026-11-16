//! Boolean conditions from the selected function's CFG and C++ operand guards.
//! No permission interpretation, feasibility decision or persistent graph mutation.
use super::*;
use analysis::cfg_graph::CfgGraph;
use std::collections::HashSet;

impl Investigation<'_> {
    fn control_gap(
        &mut self,
        subject: &ContextSubject,
        code: &'static str,
        message: impl Into<String>,
        related_locations: Vec<ContextLocation>,
    ) -> anyhow::Result<()> {
        self.reserve_item()?;
        self.result.gaps.push(CallContextGap {
            subject: subject.clone(),
            code,
            message: message.into(),
            related_locations,
        });
        Ok(())
    }

    pub(super) fn control_conditions(
        &mut self,
        file_id: FileId,
        start: u32,
        end: u32,
    ) -> anyhow::Result<()> {
        let at = ContextLocation {
            file_id,
            range: TextRange {
                start_byte: start,
                end_byte: end,
                ..Default::default()
            },
        };
        let mut subject = ContextSubject::Region {
            location: at.clone(),
            symbol_id: None,
        };
        if self
            .store
            .get_file(&file_id)?
            .is_none_or(|f| f.language != Language::Cpp)
        {
            return self.control_gap(
                &subject,
                "control_syntax_unsupported",
                "Control-condition inspection currently supports indexed C++ functions.",
                vec![],
            );
        }
        let parsed = match self.source(file_id) {
            Ok(parsed) => parsed,
            Err(error) => {
                return self.control_gap(&subject, "control_source_unavailable", error, vec![]);
            }
        };
        let Some(owner) = self.function_owner(&at, &parsed)? else {
            return self.control_gap(&subject, "control_scope_unavailable", "No unique indexed function body owns this position; inspect its source and scope limits.", vec![]);
        };
        subject = ContextSubject::Region {
            location: at.clone(),
            symbol_id: Some(owner.id),
        };
        let scope = ContextLocation {
            file_id,
            range: owner.range,
        };
        let syntax = parsed.tree.root_node().descendant_for_byte_range(
            owner.range.start_byte as usize,
            owner.range.end_byte as usize,
        );
        let mut unexamined_ranges = Vec::new();
        let mut transfers = Vec::new();
        let mut conditional_operands = Vec::new();
        let mut logical_operands = Vec::new();
        if let Some(function) = syntax {
            // Operand selection is a language fact, independent of whether the
            // statement CFG lowers this expression. Follow only the selected
            // expression's ancestry within its established evaluation scope;
            // an init-capture RHS belongs to creation, a closure body does not.
            let selected = parsed
                .tree
                .root_node()
                .descendant_for_byte_range(start as usize, end as usize);
            for node in std::iter::successors(selected, |node| node.parent())
                .take_while(|node| *node != function)
            {
                self.check()?;
                if let Some((left, right, role)) = logical_operands_of(node, &parsed.source)
                    && right.start_byte() <= start as usize
                    && end as usize <= right.end_byte()
                {
                    logical_operands.push((node, left, right, role, selected));
                }
                if node.kind() != "conditional_expression" {
                    continue;
                }
                let (Some(condition), Some(consequence), Some(alternative)) = (
                    node.child_by_field_name("condition"),
                    node.child_by_field_name("consequence"),
                    node.child_by_field_name("alternative"),
                ) else {
                    continue;
                };
                let selected_operand = [consequence, alternative].iter().position(|operand| {
                    operand.start_byte() <= start as usize && end as usize <= operand.end_byte()
                });
                if let Some(operand) = selected_operand {
                    let supported = !node.has_error()
                        && !node.is_missing()
                        && selected.is_some_and(|selected| {
                            extraction::cpp_expressions::may_be_evaluated(selected, &parsed.source)
                        });
                    conditional_operands.push((
                        cpp::range(node),
                        [condition, consequence, alternative].map(cpp::range),
                        supported.then_some(if operand == 0 {
                            "control_true_branch"
                        } else {
                            "control_false_branch"
                        }),
                    ));
                }
            }
            let mut pending = vec![function];
            while let Some(node) = pending.pop() {
                self.check()?;
                if node != function
                    && matches!(node.kind(), "function_definition" | "lambda_expression")
                {
                    continue;
                }
                if node.kind() == "goto_statement" {
                    transfers.push(cpp::range(node));
                }
                if matches!(node.kind(), "preproc_if" | "preproc_ifdef") {
                    let range = cpp::range(node);
                    self.control_gap(&subject, "control_configuration_unestablished", "No build configuration has been selected. Supported statement alternatives are retained as possible CFG paths; opaque regions remain possible bypasses. Recorded conditions do not establish configuration feasibility or macro expansion.", vec![ContextLocation { file_id, range }])?;
                    unexamined_ranges.push(range);
                    // Nested conditional regions can independently remain
                    // opaque, even when this outer alternative is represented.
                }
                let mut cursor = node.walk();
                pending.extend(node.named_children(&mut cursor));
            }
        }
        let facts = self.function_facts(&owner, &parsed, false)?;
        self.check()?;
        for diagnostic in &facts.diagnostics {
            let range = diagnostic.range.unwrap_or(owner.range);
            if diagnostic.level != DiagnosticLevel::Info
                && range.start_byte <= owner.range.end_byte
                && owner.range.start_byte <= range.end_byte
            {
                self.control_gap(
                    &subject,
                    "control_extraction_limit",
                    &diagnostic.message,
                    vec![ContextLocation { file_id, range }],
                )?;
            }
        }
        if facts.cfg_failed || facts.budget_exceeded {
            return self.control_gap(&subject, "control_analysis_incomplete", "Function CFG extraction failed or exceeded its budget; conditions are not established.", vec![scope]);
        }
        let mut operand_guards = Vec::new();
        if !logical_operands.is_empty() {
            let calls = self.store.find_callsites_by_file(&file_id)?;
            // Shared across all enclosing expressions, including recursive
            // operand checks. Exhaustion leaves a located unknown guard.
            let mut remaining = 256;
            for (node, left, right, role, selected) in logical_operands {
                let evaluated = !node.has_error()
                    && !node.is_missing()
                    && selected.is_some_and(|selected| {
                        extraction::cpp_expressions::may_be_evaluated(selected, &parsed.source)
                    });
                let supported = evaluated
                    && self.scalar_operand(left, &parsed, &facts, &calls, &mut remaining)?
                    && self.scalar_operand(right, &parsed, &facts, &calls, &mut remaining)?;
                if supported {
                    operand_guards.push((cpp::range(node), cpp::range(left), role));
                } else {
                    let (code, message) = if !evaluated {
                        (
                            "control_expression_unestablished",
                            "Short-circuit operand evaluation is not established for this syntax or evaluation context. Read both operands at the related locations.",
                        )
                    } else if remaining == 0 {
                        (
                            "control_analysis_budget",
                            "The shared 256-expression operand type-check budget is exhausted; this short-circuit guard remains unexamined. Independent established guards are retained.",
                        )
                    } else {
                        (
                            "control_short_circuit_unestablished",
                            "The recorded operand types do not establish a built-in logical operator. Class/enum operands may use overloaded operators without short-circuit evaluation; unresolved calls and unsupported types require source continuation. Read both operands; independent outer conditions remain applicable.",
                        )
                    };
                    self.control_gap(
                        &subject,
                        code,
                        message,
                        vec![loc(file_id, left), loc(file_id, right)],
                    )?;
                }
            }
        }
        let nodes: Vec<_> = facts
            .cfg_nodes
            .into_iter()
            .filter(|n| n.function_id == owner.id)
            .collect();
        for (range, operands, role) in conditional_operands {
            if !nodes
                .iter()
                .any(|node| node.kind == CfgNodeKind::Branch && node.stmt_range == range)
            {
                if let Some(role) = role {
                    operand_guards.push((range, operands[0], role));
                    continue;
                }
                self.control_gap(
                    &subject,
                    "control_expression_unestablished",
                    "The conditional operand guard is not established for this syntax or evaluation context. Read the condition and both operands at the related locations. Other recorded conditions do not establish which operand executes.",
                    operands.into_iter().map(|range| ContextLocation { file_id, range }).collect(),
                )?;
            }
        }
        let mut unexamined: HashSet<_> = nodes
            .iter()
            .filter(|n| n.kind == CfgNodeKind::Loop)
            .map(|n| n.id)
            .collect();
        if !unexamined.is_empty() {
            let loops = nodes
                .iter()
                .filter(|n| unexamined.contains(&n.id))
                .map(|n| ContextLocation {
                    file_id,
                    range: n.stmt_range,
                })
                .collect();
            self.control_gap(&subject, "control_loop_condition_unestablished", "The current CFG does not establish boolean loop edges. Loops remain possible control-flow bypasses; independent conditions may still be retained. Read the loop and surrounding branches.", loops)?;
        }
        let ids: HashSet<_> = nodes.iter().map(|n| n.id).collect();
        let edges: Vec<_> = facts
            .cfg_edges
            .into_iter()
            .filter(|e| ids.contains(&e.source) && ids.contains(&e.target))
            .collect();
        for range in transfers {
            self.check()?;
            if !nodes.iter().any(|node| {
                node.stmt_range == range
                    && edges
                        .iter()
                        .any(|edge| edge.source == node.id && edge.kind == CfgEdgeKind::Goto)
            }) {
                self.control_gap(&subject, "control_transfer_unestablished", "This written jump has no established CFG target. It remains a possible bypass; a missing or ambiguous label must not establish a dependent guard.", vec![ContextLocation { file_id, range }])?;
                unexamined_ranges.push(range);
            }
        }
        if nodes.len() > 20_000 || edges.len() > 100_000 {
            return self.control_gap(
                &subject,
                "control_analysis_budget",
                "The function exceeds the control graph budget; no conditions were computed.",
                vec![scope],
            );
        }
        for range in unexamined_ranges {
            self.check()?;
            if nodes.iter().any(|node| {
                node.kind == CfgNodeKind::Branch
                    && node.stmt_range == range
                    && edges
                        .iter()
                        .any(|edge| edge.source == node.id && edge.kind == CfgEdgeKind::CaseBranch)
            }) {
                continue;
            }
            let containing: Vec<_> = nodes
                .iter()
                .filter(|n| {
                    !matches!(
                        n.kind,
                        CfgNodeKind::Entry | CfgNodeKind::Exit | CfgNodeKind::BlockExit
                    ) && n.stmt_range.start_byte <= range.start_byte
                        && range.end_byte <= n.stmt_range.end_byte
                })
                .collect();
            let Some(width) = containing
                .iter()
                .map(|n| n.stmt_range.end_byte - n.stmt_range.start_byte)
                .min()
            else {
                return self.control_gap(&subject, "control_region_unavailable", "An unexamined control region has no corresponding CFG node; independent conditions could not be established.", vec![ContextLocation { file_id, range }]);
            };
            unexamined.extend(
                containing
                    .into_iter()
                    .filter(|n| n.stmt_range.end_byte - n.stmt_range.start_byte == width)
                    .map(|n| n.id),
            );
        }
        let containing: Vec<_> = nodes
            .iter()
            .filter(|n| {
                if n.stmt_range.start_byte > start
                    || end > n.stmt_range.end_byte
                    || n.stmt_range.start_byte == n.stmt_range.end_byte
                {
                    return false;
                }
                if matches!(n.kind, CfgNodeKind::Branch | CfgNodeKind::Loop) {
                    return parsed
                        .tree
                        .root_node()
                        .descendant_for_byte_range(
                            n.stmt_range.start_byte as usize,
                            n.stmt_range.end_byte as usize,
                        )
                        .filter(|syntax| cpp::range(*syntax) == n.stmt_range)
                        .and_then(|syntax| syntax.child_by_field_name("condition"))
                        .is_some_and(|condition| {
                            condition.start_byte() <= start as usize
                                && end as usize <= condition.end_byte()
                        });
                }
                true
            })
            .collect();
        let width = containing
            .iter()
            .map(|n| n.stmt_range.end_byte - n.stmt_range.start_byte)
            .min();
        let targets: HashSet<_> = containing
            .into_iter()
            .filter(|n| Some(n.stmt_range.end_byte - n.stmt_range.start_byte) == width)
            .map(|n| n.id)
            .collect();
        if targets.is_empty() {
            return self.control_gap(&subject, "control_region_unavailable", "No recorded CFG statement covers this selection; select an expression or statement and continue source inspection.", vec![scope]);
        }
        let graph = match CfgGraph::build(&nodes, &edges) {
            Ok(graph) => graph,
            Err(error) => {
                return self.control_gap(
                    &subject,
                    "control_analysis_incomplete",
                    error.to_string(),
                    vec![scope],
                );
            }
        };
        let required = graph.required_branches(&targets, &unexamined, 500_000, self.canceled)?;
        if required.truncated {
            self.control_gap(&subject, "control_analysis_budget", "The control traversal budget stopped analysis; remaining conditions were not examined.", vec![scope.clone()])?;
        }
        if !required.target_reachable {
            return self.control_gap(&subject, "control_reachability_unestablished", "The selection was not reached in the examined CFG. This does not prove program unreachability; inspect omitted or unsupported control flow.", vec![scope]);
        }
        for edge in required.edges {
            self.check()?;
            let branch = &graph.nodes[&edge.source];
            let syntax = parsed
                .tree
                .root_node()
                .descendant_for_byte_range(
                    branch.stmt_range.start_byte as usize,
                    branch.stmt_range.end_byte as usize,
                )
                .filter(|n| cpp::range(*n) == branch.stmt_range);
            let condition = syntax
                .and_then(|n| n.child_by_field_name("condition"))
                .filter(|n| !n.has_error() && !n.is_missing());
            let Some(condition) = condition else {
                self.control_gap(
                    &subject,
                    "control_condition_unavailable",
                    "A required CFG branch has no corresponding written boolean condition.",
                    vec![ContextLocation {
                        file_id,
                        range: branch.stmt_range,
                    }],
                )?;
                continue;
            };
            let role = if edge.kind == CfgEdgeKind::TrueBranch {
                "control_true_branch"
            } else {
                "control_false_branch"
            };
            self.reserve_item()?;
            self.result.items.push(CallContextItem {
                kind: ContextItemKind::ControlCondition,
                subject: subject.clone(), role,
                location: ContextLocation { file_id, range: cpp::range(condition) },
                symbol_id: None,
                related_locations: vec![scope.clone(), ContextLocation { file_id, range: branch.stmt_range }],
                message: "Every recorded CFG path from function entry to this selection traverses this boolean branch edge. This is a path observation, not the condition's current value or a runtime permission decision.".into(),
            });
        }
        for (expression, condition, role) in operand_guards {
            self.check()?;
            self.reserve_item()?;
            self.result.items.push(CallContextItem {
                kind: ContextItemKind::ControlCondition,
                subject: subject.clone(), role,
                location: ContextLocation { file_id, range: condition },
                symbol_id: None,
                related_locations: vec![scope.clone(), ContextLocation { file_id, range: expression }],
                message: "Evaluating this selected operand of a supported C++ conditional or built-in logical expression requires the indicated condition outcome. This source rule does not establish the condition's value, operand execution or a runtime permission decision.".into(),
            });
        }
        self.control_gap(&subject, "control_flow_limited", "Conditions describe this function's recorded CFG and supported source operand guards. Unsupported or omitted flow, callee effects, exceptions, runtime feasibility and caller permissions remain unestablished; no conditions does not mean unrestricted execution.", vec![scope])
    }

    /// A bounded type check, not value evaluation or overload resolution.
    /// [over.match.oper] permits the built-in rule when neither operand has
    /// class or enumeration type. Unknown types must not select that rule.
    fn scalar_operand(
        &mut self,
        node: Node<'_>,
        parsed: &ParsedSource,
        facts: &FileFacts,
        calls: &[Callsite],
        remaining: &mut usize,
    ) -> anyhow::Result<bool> {
        self.check()?;
        if *remaining == 0 || node.has_error() || node.is_missing() {
            return Ok(false);
        }
        *remaining -= 1;
        match node.kind() {
            "true" | "false" | "null" => Ok(matches!(
                cpp::text(node, &parsed.source),
                "true" | "false" | "nullptr"
            )),
            "parenthesized_expression" => match node.named_child(0) {
                Some(inner) => self.scalar_operand(inner, parsed, facts, calls, remaining),
                None => Ok(false),
            },
            "unary_expression"
                if node
                    .child_by_field_name("operator")
                    .is_some_and(|op| matches!(cpp::text(op, &parsed.source), "!" | "not")) =>
            {
                match node.child_by_field_name("argument") {
                    Some(inner) => self.scalar_operand(inner, parsed, facts, calls, remaining),
                    None => Ok(false),
                }
            }
            "binary_expression" => {
                let Some((left, right, _)) = logical_operands_of(node, &parsed.source) else {
                    return Ok(false);
                };
                Ok(self.scalar_operand(left, parsed, facts, calls, remaining)?
                    && self.scalar_operand(right, parsed, facts, calls, remaining)?)
            }
            "identifier" => {
                let Some(types) = &parsed.cpp else {
                    return Ok(false);
                };
                let name = cpp::text(node, &parsed.source);
                if types.macros.iter().any(|m| m.name == name)
                    || types
                        .lookup_limits
                        .iter()
                        .any(|limit| limit.limits_local_lookup(name, node.start_byte() as u32))
                {
                    return Ok(false);
                }
                let uses: Vec<_> = facts
                    .binding_uses
                    .iter()
                    .filter(|usage| usage.range == cpp::range(node))
                    .collect();
                let [usage] = uses.as_slice() else {
                    return Ok(false);
                };
                let Some(id) = usage.binding_id else {
                    return Ok(false);
                };
                let values: Vec<_> = types
                    .values
                    .iter()
                    .filter(|value| value.binding_id == Some(id))
                    .collect();
                Ok(
                    matches!(values.as_slice(), [value] if value.declared_type.as_ref().is_some_and(scalar_type)),
                )
            }
            "call_expression" => {
                let matches: Vec<_> = calls
                    .iter()
                    .filter(|call| call.range == cpp::range(node))
                    .collect();
                let [call] = matches.as_slice() else {
                    return Ok(false);
                };
                let resolved = self.store.find_resolved_callsites_by_id(&call.id)?;
                let [resolved] = resolved.as_slice() else {
                    return Ok(false);
                };
                let Some(callee) = self.store.find_symbol_by_id(&resolved.callee)? else {
                    return Ok(false);
                };
                let Ok(source) = self.source(callee.file_id) else {
                    return Ok(false);
                };
                Ok(source
                    .cpp
                    .as_ref()
                    .and_then(|types| {
                        types
                            .callables
                            .iter()
                            .find(|decl| decl.symbol_id == callee.id)
                    })
                    .and_then(|decl| decl.return_type.as_ref())
                    .is_some_and(scalar_type))
            }
            _ => Ok(false),
        }
    }
}

fn logical_operands_of<'a>(
    node: Node<'a>,
    source: &str,
) -> Option<(Node<'a>, Node<'a>, &'static str)> {
    if node.kind() != "binary_expression" {
        return None;
    }
    let role = match cpp::text(node.child_by_field_name("operator")?, source) {
        "&&" | "and" => "control_true_branch",
        "||" | "or" => "control_false_branch",
        _ => return None,
    };
    Some((
        node.child_by_field_name("left")?,
        node.child_by_field_name("right")?,
        role,
    ))
}

fn scalar_type(ty: &types::cpp::CppDeclaredType) -> bool {
    ty.template_arguments.is_empty()
        && !ty.name.is_empty()
        && ty.name.split_whitespace().all(|word| {
            matches!(
                word,
                "bool"
                    | "char"
                    | "wchar_t"
                    | "char8_t"
                    | "char16_t"
                    | "char32_t"
                    | "short"
                    | "int"
                    | "long"
                    | "signed"
                    | "unsigned"
                    | "float"
                    | "double"
            )
        })
}
