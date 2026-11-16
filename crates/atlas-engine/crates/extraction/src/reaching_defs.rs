//! May-reaching definitions of recorded local writes over the existing CFG.
//!
//! This computes definition identity, not value equality, alias effects, path
//! feasibility or interprocedural propagation. A definite local write kills
//! earlier definitions only on paths which complete that write normally.

use std::collections::{HashMap, HashSet};

use tree_sitter::Node;
use types::{
    CfgEdgeKind, CfgNodeKind, DataFlowEdge, DataFlowEdgeId, DataFlowKind, DataNode, DataNodeId,
    DataNodeKind, DiagnosticLevel, ExtractDiagnostic, SymbolId, TextRange,
};

use crate::dataflow_builder::use_def_key;
use crate::extraction_ctx::ExtractionCtx;
use crate::{CancelCheck, CfgResult};

#[derive(Default)]
pub struct UseDefResult {
    pub edges: Vec<DataFlowEdge>,
    pub diagnostics: Vec<ExtractDiagnostic>,
    pub truncated: bool,
}

struct Write<'a> {
    node: &'a DataNode,
    activation: u32,
    definite: bool,
}

/// Returns None on cancellation. Work exhaustion preserves completed facts and
/// explicitly marks the rest of this file's use-def computation unexamined.
pub(crate) fn resolve(
    nodes: &[DataNode],
    existing: &[DataFlowEdge],
    cfg: &CfgResult,
    ctx: &ExtractionCtx<'_>,
    cancel: &dyn CancelCheck,
) -> Option<UseDefResult> {
    resolve_with_budget(nodes, existing, cfg, ctx, cancel, 1_000_000)
}

fn resolve_with_budget(
    nodes: &[DataNode],
    existing: &[DataFlowEdge],
    cfg: &CfgResult,
    ctx: &ExtractionCtx<'_>,
    cancel: &dyn CancelCheck,
    mut work_left: usize,
) -> Option<UseDefResult> {
    let mut result = UseDefResult::default();
    let by_id: HashMap<_, _> = nodes.iter().map(|n| (n.id, n)).collect();
    let mut activation: HashMap<_, _> = nodes.iter().map(|n| (n.id, n.range.end_byte)).collect();
    for edge in existing.iter().filter(|e| e.kind == DataFlowKind::Assign) {
        if let Some(end) = activation.get_mut(&edge.target) {
            *end = (*end).max(
                by_id
                    .get(&edge.source)
                    .map_or(edge.location.end_byte, |n| n.range.end_byte),
            );
        }
    }
    // The explicit value-construction edges establish a narrower order than
    // a strong update: a Read operand is consumed before the aggregate value
    // is assigned. Even when overwrite effects are unknown, the output of
    // this evaluation cannot feed its own input. A CFG backedge may still
    // carry the same recorded definition from a previous loop iteration.
    let mut value_reads: HashMap<DataNodeId, Vec<DataNodeId>> = HashMap::new();
    for edge in existing.iter().filter(|e| e.kind == DataFlowKind::Read) {
        if cancel.is_cancelled() {
            return None;
        }
        if !spend_work(&mut work_left, 1, &mut result) {
            return Some(result);
        }
        if let (Some(input), Some(value)) = (by_id.get(&edge.source), by_id.get(&edge.target))
            && input.kind == DataNodeKind::VariableUse
            && value.kind == DataNodeKind::Expr
            && input.function_id == value.function_id
            && value.range.start_byte <= input.range.start_byte
            && input.range.end_byte <= value.range.end_byte
        {
            value_reads.entry(value.id).or_default().push(input.id);
        }
    }
    let mut read_before_write = HashSet::new();
    let mut binding_nodes: HashMap<_, Vec<_>> = HashMap::new();
    for node in nodes.iter().filter(|n| n.binding_id.is_some()) {
        binding_nodes
            .entry((node.function_id, node.binding_id))
            .or_default()
            .push(node);
    }
    for output in nodes.iter().filter(|n| n.kind == DataNodeKind::CallOutput) {
        if cancel.is_cancelled() {
            return None;
        }
        if let Some(call) = crate::call_outputs::call_range(output, ctx.root) {
            activation.insert(output.id, call.end_byte);
            // A call body cannot supply values to its own argument evaluation.
            // A backedge can still carry a prior invocation's possible write.
            for input in binding_nodes
                .get(&(output.function_id, output.binding_id))
                .into_iter()
                .flatten()
            {
                if !spend_work(&mut work_left, 1, &mut result) {
                    return Some(result);
                }
                if call.start_byte <= input.range.start_byte
                    && input.range.end_byte <= call.end_byte
                {
                    read_before_write.insert((input.id, output.id));
                }
            }
        }
    }
    for edge in existing.iter().filter(|e| e.kind == DataFlowKind::Assign) {
        if let Some(inputs) = value_reads.get(&edge.source)
            && by_id
                .get(&edge.target)
                .is_some_and(|n| n.kind == DataNodeKind::Local)
        {
            for input in inputs {
                if cancel.is_cancelled() {
                    return None;
                }
                if !spend_work(&mut work_left, 1, &mut result) {
                    return Some(result);
                }
                read_before_write.insert((*input, edge.target));
            }
        }
    }
    let cfg_ids: HashMap<_, _> = cfg
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id, i))
        .collect();
    let mut predecessors = vec![Vec::new(); cfg.nodes.len()];
    let mut successors = vec![Vec::new(); cfg.nodes.len()];
    for edge in &cfg.edges {
        if let (Some(&from), Some(&to)) = (cfg_ids.get(&edge.source), cfg_ids.get(&edge.target))
            && cfg.nodes[from].function_id == cfg.nodes[to].function_id
        {
            predecessors[to].push((from, edge.kind != CfgEdgeKind::Exception));
            successors[from].push(to);
        }
    }
    // Direct-goto lowering can retain disconnected statements. A backwards
    // walk must not treat their outgoing edges as entry-reachable paths.
    let mut reachable = HashSet::new();
    let mut pending: Vec<_> = cfg
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.kind == CfgNodeKind::Entry)
        .map(|(i, _)| i)
        .collect();
    while let Some(i) = pending.pop() {
        if cancel.is_cancelled() {
            return None;
        }
        if !spend_work(&mut work_left, 1, &mut result) {
            return Some(result);
        }
        if reachable.insert(i) {
            pending.extend(&successors[i]);
        }
    }
    for entries in &mut predecessors {
        entries.retain(|(i, _)| reachable.contains(i));
    }
    let mut by_function: HashMap<SymbolId, Vec<usize>> = HashMap::new();
    for (i, node) in cfg.nodes.iter().enumerate() {
        by_function.entry(node.function_id).or_default().push(i);
    }
    let mut groups: HashMap<_, Vec<_>> = HashMap::new();
    for node in nodes {
        if cfg.terminated_tails.iter().any(|(function, range)| {
            node.function_id == Some(*function)
                && range.start_byte <= node.range.start_byte
                && node.range.end_byte <= range.end_byte
        }) {
            continue;
        }
        if let Some(key) = use_def_key(node) {
            groups.entry(key).or_default().push(node);
        }
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by_key(|group| group.iter().map(|n| (n.range.start_byte, n.id)).min());
    let mut diagnostics = HashSet::new();
    let mut edge_ids = HashSet::new();
    for group in groups {
        if cancel.is_cancelled() {
            return None;
        }
        let definitions: Vec<_> = group
            .iter()
            .copied()
            .filter(|n| {
                matches!(
                    n.kind,
                    DataNodeKind::Local | DataNodeKind::Parameter | DataNodeKind::CallOutput
                )
            })
            .collect();
        let reads: Vec<_> = group
            .iter()
            .copied()
            .filter(|n| {
                matches!(
                    n.kind,
                    DataNodeKind::VariableUse
                        | DataNodeKind::ParameterOutput
                        | DataNodeKind::Receiver
                        | DataNodeKind::Expr
                        | DataNodeKind::CallArg
                        | DataNodeKind::Return
                )
            })
            .collect();
        if definitions.is_empty() || reads.is_empty() {
            continue;
        }
        let function_cfg = group[0]
            .function_id
            .and_then(|id| by_function.get(&id))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut writes: HashMap<usize, Vec<Write<'_>>> = HashMap::new();
        let mut unplaced = Vec::new();
        let builtin_assignment = ctx.language != types::Language::Cpp
            || definitions.iter().any(|n| has_builtin_value_type(n, ctx));
        for definition in &definitions {
            if cancel.is_cancelled() {
                return None;
            }
            if !spend_work(&mut work_left, function_cfg.len().max(1), &mut result) {
                return Some(result);
            }
            let owners = owners(definition, function_cfg, cfg);
            if owners.is_empty() {
                unplaced.push(*definition);
                diagnose(
                    &mut result,
                    &mut diagnostics,
                    definition.range,
                    "use_def_cfg_unavailable: recorded definition has no corresponding CFG region; its possible uses remain unordered",
                );
            }
            for owner in owners {
                let definite = definition.kind == DataNodeKind::Parameter
                    || (definition.kind != DataNodeKind::CallOutput
                        && definition.binding_id.is_some()
                        && unconditional_write(
                            definition,
                            &cfg.nodes[owner],
                            ctx.root,
                            builtin_assignment,
                        ));
                if !definite && definition.kind != DataNodeKind::CallOutput {
                    diagnose(
                        &mut result,
                        &mut diagnostics,
                        definition.range,
                        "use_def_write_order_unmodeled: this write's overwrite semantics or execution/order is not established within its CFG region; prior definitions are retained",
                    );
                }
                writes.entry(owner).or_default().push(Write {
                    node: definition,
                    activation: activation[&definition.id],
                    definite,
                });
            }
        }
        for entries in writes.values_mut() {
            entries.sort_by_key(|w| {
                std::cmp::Reverse((w.activation, w.node.range.start_byte, w.node.id))
            });
        }
        for read in reads {
            if cancel.is_cancelled() {
                return None;
            }
            if !spend_work(
                &mut work_left,
                function_cfg.len().max(definitions.len()).max(1),
                &mut result,
            ) {
                return Some(result);
            }
            let read_owners = owners(read, function_cfg, cfg);
            if read.kind == DataNodeKind::ParameterOutput && read_owners.is_empty() {
                diagnose(
                    &mut result,
                    &mut diagnostics,
                    read.range,
                    "use_def_cfg_unavailable: reference output has no recorded CFG exit; normal-exit definitions remain unavailable",
                );
                continue;
            }
            let mut reaching = HashSet::new();
            reaching.extend(unplaced.iter().map(|n| n.id));
            if read_owners.is_empty() {
                reaching.extend(definitions.iter().map(|n| n.id));
                diagnose(
                    &mut result,
                    &mut diagnostics,
                    read.range,
                    "use_def_cfg_unavailable: use has no corresponding CFG region; recorded definitions remain unordered",
                );
            }
            let mut pending: Vec<_> = read_owners
                .into_iter()
                .filter(|i| reachable.contains(i))
                .map(|i| {
                    (
                        i,
                        if read.kind == DataNodeKind::ParameterOutput {
                            u32::MAX
                        } else {
                            read.range.start_byte
                        },
                        true,
                    )
                })
                .collect();
            let mut visited = HashSet::new();
            while let Some((current, cutoff, normal)) = pending.pop() {
                if cancel.is_cancelled() {
                    return None;
                }
                if !visited.insert((current, cutoff, normal)) {
                    continue;
                }
                if !spend_work(&mut work_left, 1, &mut result) {
                    return Some(result);
                }
                let mut killed = false;
                if let Some(entries) = writes.get(&current) {
                    for write in entries {
                        if !spend_work(&mut work_left, 1, &mut result) {
                            return Some(result);
                        }
                        // Unknown expression ordering cannot exclude a write merely
                        // because its token appears after this read. Entry parameters
                        // are already available before the first body statement.
                        let parameter = write.node.kind == DataNodeKind::Parameter;
                        if cutoff == read.range.start_byte
                            && read_before_write.contains(&(read.id, write.node.id))
                        {
                            continue;
                        }
                        if parameter || !write.definite || write.activation <= cutoff {
                            reaching.insert(write.node.id);
                            if write.definite && normal && !parameter {
                                killed = true;
                                break;
                            }
                        }
                    }
                }
                if !killed {
                    pending.extend(
                        predecessors[current]
                            .iter()
                            .map(|&(i, normal)| (i, u32::MAX, normal)),
                    );
                }
            }
            let mut reaching: Vec<_> = reaching.into_iter().collect();
            reaching.sort();
            for definition in reaching {
                let id =
                    DataFlowEdgeId::generate(&definition, &read.id, DataFlowKind::Assign.as_str());
                if edge_ids.insert(id) {
                    result.edges.push(DataFlowEdge::new(
                        id,
                        definition,
                        read.id,
                        DataFlowKind::Assign,
                        read.range,
                        0.85,
                    ));
                }
            }
        }
    }
    Some(result)
}

fn owners(node: &DataNode, function_cfg: &[usize], cfg: &CfgResult) -> Vec<usize> {
    if node.kind == DataNodeKind::ParameterOutput {
        return function_cfg
            .iter()
            .copied()
            .filter(|&i| {
                cfg.normal_exit_sources.contains(&cfg.nodes[i].id)
                    && crate::call_outputs::parameter_output_id(node, &cfg.nodes[i]) == node.id
            })
            .collect();
    }
    if node.kind == DataNodeKind::Parameter {
        return function_cfg
            .iter()
            .copied()
            .filter(|&i| cfg.nodes[i].kind == CfgNodeKind::Entry)
            .collect();
    }
    let mut matches: Vec<_> = function_cfg
        .iter()
        .copied()
        .filter(|&i| {
            let c = &cfg.nodes[i];
            !matches!(
                c.kind,
                CfgNodeKind::Entry | CfgNodeKind::Exit | CfgNodeKind::BlockExit
            ) && c.stmt_range.start_byte <= node.range.start_byte
                && node.range.end_byte <= c.stmt_range.end_byte
                && c.stmt_range.start_byte < c.stmt_range.end_byte
        })
        .collect();
    if let Some(min) = matches
        .iter()
        .map(|&i| cfg.nodes[i].stmt_range.end_byte - cfg.nodes[i].stmt_range.start_byte)
        .min()
    {
        // Finally/resource lowering can have several path-isolated instances of
        // the same statement. All of those instances participate in the query.
        matches.retain(|&i| {
            cfg.nodes[i].stmt_range.end_byte - cfg.nodes[i].stmt_range.start_byte == min
        });
    }
    matches
}

fn unconditional_write(
    definition: &DataNode,
    cfg: &types::CfgNode,
    root: Node<'_>,
    builtin_assignment: bool,
) -> bool {
    if cfg.kind != CfgNodeKind::Statement {
        return false;
    }
    let Some(target) = root.descendant_for_byte_range(
        definition.range.start_byte as usize,
        definition.range.end_byte as usize,
    ) else {
        return false;
    };
    if target.kind() != "identifier"
        || target.start_byte() != definition.range.start_byte as usize
        || target.end_byte() != definition.range.end_byte as usize
    {
        return false;
    }
    let Some(mut write) = target.parent() else {
        return false;
    };
    let target_field = match write.kind() {
        "assignment_expression" | "assignment" | "augmented_assignment" => "left",
        "init_declarator" => "declarator",
        "variable_declarator" => "name",
        "update_expression" => "argument",
        _ => return false,
    };
    if !builtin_assignment && !matches!(write.kind(), "init_declarator" | "variable_declarator") {
        return false;
    }
    if write
        .child_by_field_name(target_field)
        .is_none_or(|n| n.id() != target.id())
    {
        return false;
    }
    loop {
        if write.has_error() || write.is_missing() {
            return false;
        }
        if write.start_byte() == cfg.stmt_range.start_byte as usize
            && write.end_byte() == cfg.stmt_range.end_byte as usize
        {
            return true;
        }
        let Some(parent) = write.parent() else {
            return false;
        };
        if !matches!(
            parent.kind(),
            "expression_statement"
                | "parenthesized_expression"
                | "declaration"
                | "lexical_declaration"
                | "variable_declaration"
        ) {
            return false;
        }
        write = parent;
    }
}

/// C++ class assignment can invoke an operator which retains the old state.
/// Only an explicitly written built-in value type establishes ordinary local
/// overwrite here, including the referent of a reference parameter. Auto,
/// aliases, local references and user-defined types need more
/// semantics; they remain weak writes rather than starting a new type solver.
fn has_builtin_value_type(node: &DataNode, ctx: &ExtractionCtx<'_>) -> bool {
    let Some(target) = ctx
        .root
        .descendant_for_byte_range(node.range.start_byte as usize, node.range.end_byte as usize)
    else {
        return false;
    };
    let Some(mut declaration) = target.parent() else {
        return false;
    };
    if node.kind == DataNodeKind::Parameter && declaration.kind() == "reference_declarator" {
        let Some(parent) = declaration.parent() else {
            return false;
        };
        declaration = parent;
    }
    if declaration.kind() == "init_declarator" {
        let Some(parent) = declaration.parent() else {
            return false;
        };
        declaration = parent;
    }
    if !matches!(declaration.kind(), "declaration" | "parameter_declaration")
        || declaration.has_error()
    {
        return false;
    }
    let Some(ty) = declaration.child_by_field_name("type") else {
        return false;
    };
    let Ok(text) = ty.utf8_text(ctx.source_bytes()) else {
        return false;
    };
    let mut tokens = text.split_whitespace().peekable();
    tokens.peek().is_some()
        && tokens.all(|s| {
            matches!(
                s,
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

fn diagnose(
    result: &mut UseDefResult,
    seen: &mut HashSet<(u32, u32, &'static str)>,
    range: TextRange,
    message: &'static str,
) {
    if seen.insert((range.start_byte, range.end_byte, message)) {
        result.diagnostics.push(ExtractDiagnostic {
            level: DiagnosticLevel::Warning,
            message: message.into(),
            range: Some(range),
        });
    }
}

fn spend_work(left: &mut usize, amount: usize, result: &mut UseDefResult) -> bool {
    if let Some(remaining) = left.checked_sub(amount) {
        *left = remaining;
        true
    } else {
        result.truncated = true;
        result.diagnostics.push(ExtractDiagnostic {
            level: DiagnosticLevel::Warning,
            message: "use_def_budget_exceeded: current and remaining use-def queries in this file are unexamined".into(),
            range: None,
        });
        false
    }
}

#[cfg(all(test, feature = "cpp"))]
mod tests {
    use super::*;
    use crate::{ExtractionMode, create_frontend, extract_file_with_mode};
    use std::{cell::Cell, path::Path};
    use types::{FileId, Language};

    #[test]
    fn reference_output_requires_a_recorded_cfg_exit() {
        let source = "void f(int& value) { value = 7; observe(value); }";
        let frontend = create_frontend(Language::Cpp).unwrap();
        let file_id = FileId::generate("exit.cpp");
        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            Path::new("exit.cpp"),
            source,
            "test",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();
        let mut parser = tree_sitter::Parser::new();
        let ts_lang = frontend.parser.tree_sitter_language();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let ctx = ExtractionCtx {
            ts_lang: &ts_lang,
            root: tree.root_node(),
            source,
            file_id,
            file_path: Path::new("exit.cpp"),
            language: Language::Cpp,
        };
        let output = facts
            .data_nodes
            .iter()
            .find(|n| n.kind == DataNodeKind::ParameterOutput)
            .unwrap();
        let missing = resolve(
            &facts.data_nodes,
            &facts.dataflow_edges,
            &CfgResult::default(),
            &ctx,
            &(),
        )
        .unwrap();
        assert!(
            missing.edges.iter().all(|e| e.target != output.id),
            "Unordered definitions do not establish reference state at normal exit"
        );
        assert!(
            !missing.edges.is_empty(),
            "Ordinary uses must retain their possible definitions without CFG"
        );
        assert!(missing.diagnostics.iter().any(|d| {
            d.range == Some(output.range) && d.message.starts_with("use_def_cfg_unavailable:")
        }));
        assert!(!missing.truncated);
    }

    #[test]
    fn use_def_missing_cfg_budget_and_cancellation_are_explicit() {
        let source = "int f(int input) { int value = input; return value; }";
        let frontend = create_frontend(Language::Cpp).unwrap();
        let facts = extract_file_with_mode(
            &frontend,
            FileId::generate("budget.cpp"),
            Path::new("budget.cpp"),
            source,
            "test",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let ts_lang = frontend.parser.tree_sitter_language();
        let ctx = ExtractionCtx {
            ts_lang: &ts_lang,
            root: tree.root_node(),
            source,
            file_id: FileId::generate("budget.cpp"),
            file_path: Path::new("budget.cpp"),
            language: Language::Cpp,
        };
        let cfg = CfgResult {
            nodes: facts.cfg_nodes,
            edges: facts.cfg_edges,
            terminated_tails: vec![],
            normal_exit_sources: vec![],
        };
        let limited =
            resolve_with_budget(&facts.data_nodes, &facts.dataflow_edges, &cfg, &ctx, &(), 0)
                .unwrap();
        assert!(limited.truncated);
        assert!(limited.edges.is_empty());
        assert_eq!(limited.diagnostics.len(), 1);
        assert!(
            limited.diagnostics[0]
                .message
                .starts_with("use_def_budget_exceeded:")
        );

        let missing = resolve(
            &facts.data_nodes,
            &facts.dataflow_edges,
            &CfgResult::default(),
            &ctx,
            &(),
        )
        .unwrap();
        assert!(
            !missing.edges.is_empty(),
            "missing CFG must retain possible definitions"
        );
        assert!(!missing.diagnostics.is_empty());
        assert!(
            missing
                .diagnostics
                .iter()
                .all(|d| d.message.starts_with("use_def_cfg_unavailable:") && d.range.is_some())
        );
        assert!(
            !missing.truncated,
            "missing analysis inputs differ from exhausted work budget"
        );

        struct StopAfter(Cell<usize>);
        impl CancelCheck for StopAfter {
            fn is_cancelled(&self) -> bool {
                let count = self.0.get();
                self.0.set(count + 1);
                count >= 2
            }
        }
        assert!(
            resolve(
                &facts.data_nodes,
                &facts.dataflow_edges,
                &cfg,
                &ctx,
                &StopAfter(Cell::new(0))
            )
            .is_none()
        );
    }
}
