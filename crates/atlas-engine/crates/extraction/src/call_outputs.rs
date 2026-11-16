//! Potential storage effects of C++ arguments, separate from their input values.
//! Target declarations can later disprove exposure (e.g. scalar by-value inputs).
//! Extraction does not claim that a call actually writes the exposed binding.
use std::collections::HashMap;
use tree_sitter::Node;
use types::{DataNode, DataNodeId, DataNodeKind, Language, TextRange};

use crate::extraction_ctx::ExtractionCtx;

fn unparen(mut node: Node<'_>) -> Node<'_> {
    while node.kind() == "parenthesized_expression" {
        let mut cursor = node.walk();
        let children: Vec<_> = node
            .named_children(&mut cursor)
            .filter(|n| n.kind() != "comment")
            .collect();
        let [inner] = children.as_slice() else { break };
        node = *inner;
    }
    node
}

/// Written reference binding of a C++ parameter, independent of resolving its
/// type. Names in nested function-type parameters do not name this parameter.
pub fn is_cpp_reference_parameter(parameter: Node<'_>) -> bool {
    if !matches!(
        parameter.kind(),
        "parameter_declaration" | "optional_parameter_declaration"
    ) || parameter.has_error()
    {
        return false;
    }
    let mut pending: Vec<_> = parameter
        .child_by_field_name("declarator")
        .into_iter()
        .collect();
    while let Some(node) = pending.pop() {
        if matches!(
            node.kind(),
            "parameter_declaration" | "optional_parameter_declaration"
        ) {
            continue;
        }
        if node.kind() == "identifier" {
            let mut current = node;
            let mut reference = false;
            while let Some(parent) = current.parent() {
                if parent
                    .child_by_field_name("declarator")
                    .is_some_and(|n| n != current)
                {
                    break; // Array bounds or other expressions are not the binding.
                }
                if parent == parameter {
                    return reference;
                }
                reference |= parent.kind() == "reference_declarator";
                current = parent;
            }
        }
        let mut cursor = node.walk();
        pending.extend(
            node.named_children(&mut cursor)
                .filter(|n| n.kind() != "comment"),
        );
    }
    false
}

pub(crate) fn call_range(node: &DataNode, root: Node<'_>) -> Option<TextRange> {
    let mut syntax = root
        .descendant_for_byte_range(node.range.start_byte as usize, node.range.end_byte as usize)?;
    while let Some(parent) = syntax.parent() {
        if parent.kind() == "argument_list" {
            let call = parent.parent()?;
            return (call.kind() == "call_expression").then(|| TextRange {
                start_byte: call.start_byte() as u32,
                end_byte: call.end_byte() as u32,
                start_line: call.start_position().row as u32,
                start_column: call.start_position().column as u32,
                end_line: call.end_position().row as u32,
                end_column: call.end_position().column as u32,
            });
        }
        if matches!(parent.kind(), "function_definition" | "lambda_expression") {
            break;
        }
        syntax = parent;
    }
    None
}

pub(crate) fn append(ctx: &ExtractionCtx<'_>, nodes: &mut Vec<DataNode>) {
    if ctx.language != Language::Cpp {
        return;
    }
    let mut outputs = Vec::new();
    let bindings: HashMap<_, _> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::VariableUse && n.binding_id.is_some())
        .map(|n| ((n.function_id, n.range.start_byte, n.range.end_byte), n))
        .collect();
    for arg in nodes.iter().filter(|n| n.kind == DataNodeKind::CallArg) {
        if arg.function_id.is_none()
            || arg.callsite_id.is_none()
            || call_range(arg, ctx.root).is_none()
        {
            continue;
        }
        let Some(syntax) = ctx
            .root
            .descendant_for_byte_range(arg.range.start_byte as usize, arg.range.end_byte as usize)
        else {
            continue;
        };
        let mut value = unparen(syntax);
        if value.kind() == "pointer_expression"
            && value
                .child_by_field_name("operator")
                .is_some_and(|op| op.utf8_text(ctx.source_bytes()).ok() == Some("&"))
        {
            let Some(operand) = value.child_by_field_name("argument") else {
                continue;
            };
            value = unparen(operand);
        }
        if value.kind() != "identifier" {
            continue;
        }
        let Some(binding) = bindings.get(&(
            arg.function_id,
            value.start_byte() as u32,
            value.end_byte() as u32,
        )) else {
            continue;
        };
        let mut output = (*binding).clone();
        output.kind = DataNodeKind::CallOutput;
        output.id = DataNodeId::generate(
            &arg.file_id,
            arg.function_id.as_ref(),
            "call_output",
            binding.name.as_deref(),
            None,
            arg.range.start_byte,
        );
        output.range = arg.range;
        output.callsite_id = arg.callsite_id;
        output.arg_index = arg.arg_index;
        outputs.push(output);
    }
    nodes.extend(outputs);
}

pub(crate) fn parameter_output_id(parameter: &DataNode, exit: &types::CfgNode) -> DataNodeId {
    DataNodeId::generate(
        &parameter.file_id,
        parameter.function_id.as_ref(),
        "parameter_output",
        parameter.name.as_deref(),
        Some(&exit.id.to_string()),
        exit.stmt_range.start_byte,
    )
}

/// Each normal exit has its own use of a reference binding. This preserves
/// return/output correspondence without assuming any caller return condition.
pub(crate) fn append_parameter_outputs(
    ctx: &ExtractionCtx<'_>,
    cfg: &crate::CfgResult,
    nodes: &mut Vec<DataNode>,
    diagnostics: &mut Vec<types::ExtractDiagnostic>,
    cancel: &dyn crate::CancelCheck,
) -> Option<bool> {
    if ctx.language != Language::Cpp {
        return Some(false);
    }
    let parameters: Vec<_> = nodes
        .iter()
        .filter(|n| n.kind == DataNodeKind::Parameter && n.binding_id.is_some())
        .filter(|n| {
            ctx.root
                .descendant_for_byte_range(n.range.start_byte as usize, n.range.end_byte as usize)
                .is_some_and(|name| {
                    std::iter::successors(name.parent(), |n| n.parent())
                        .find(|n| {
                            matches!(
                                n.kind(),
                                "parameter_declaration" | "optional_parameter_declaration"
                            )
                        })
                        .is_some_and(is_cpp_reference_parameter)
                })
        })
        .cloned()
        .collect();
    let normal: std::collections::HashSet<_> = cfg.normal_exit_sources.iter().copied().collect();
    let mut exits: HashMap<_, Vec<_>> = HashMap::new();
    for node in &cfg.nodes {
        if normal.contains(&node.id) {
            exits.entry(node.function_id).or_default().push(node);
        }
    }
    let mut counts = HashMap::new();
    let mut truncated = false;
    for parameter in parameters {
        if cancel.is_cancelled() {
            return None;
        }
        let Some(function) = parameter.function_id else {
            continue;
        };
        let count = counts.entry(function).or_insert(0);
        for exit in exits.get(&function).into_iter().flatten() {
            if cancel.is_cancelled() {
                return None;
            }
            // Bound the parameter × exit product before use-def construction.
            if *count >= crate::mode::LAZY_MAX_NODES_PER_UNIT {
                if *count == crate::mode::LAZY_MAX_NODES_PER_UNIT {
                    diagnostics.push(types::ExtractDiagnostic {
                        level: types::DiagnosticLevel::Warning,
                        message: "reference_exit_budget_exceeded: remaining parameter/exit states in this function were not examined".into(),
                        range: Some(parameter.range),
                    });
                    *count += 1;
                }
                truncated = true;
                break;
            }
            let mut output = parameter.clone();
            output.kind = DataNodeKind::ParameterOutput;
            output.id = parameter_output_id(&parameter, exit);
            if exit.stmt_range.start_byte < exit.stmt_range.end_byte {
                output.range = exit.stmt_range;
            }
            nodes.push(output);
            *count += 1;
        }
    }
    Some(truncated)
}

#[cfg(all(test, feature = "cpp"))]
mod tests {
    use super::*;
    use crate::{ExtractionMode, create_frontend, extract_file_with_mode};
    use std::path::Path;
    use types::{DataFlowKind, FileId};

    #[test]
    fn reference_binding_does_not_require_resolving_the_parameter_type() {
        let source = "template<class T> void copy(T& result, T input, void (*callback)(int& nested), int (&array)[N]);\n";
        let frontend = create_frontend(Language::Cpp).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&frontend.parser.tree_sitter_language())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut pending = vec![tree.root_node()];
        let mut parameters = Vec::new();
        while let Some(node) = pending.pop() {
            if node.kind() == "parameter_declaration" {
                parameters.push((
                    node.utf8_text(source.as_bytes()).unwrap(),
                    is_cpp_reference_parameter(node),
                ));
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
        for (name, expected) in [
            ("T& result", true),
            ("T input", false),
            ("void (*callback)(int& nested)", false),
            ("int (&array)[N]", true),
        ] {
            assert!(parameters.contains(&(name, expected)), "{parameters:?}");
        }
    }

    #[test]
    fn reference_exit_product_is_bounded_before_use_def_and_can_cancel() {
        let source = "void fill(int& output) { return; }";
        let frontend = create_frontend(Language::Cpp).unwrap();
        let file_id = FileId::generate("bounded.cpp");
        let facts = extract_file_with_mode(
            &frontend,
            file_id,
            Path::new("bounded.cpp"),
            source,
            "test",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();
        let parameter = facts
            .data_nodes
            .iter()
            .find(|n| n.kind == DataNodeKind::Parameter)
            .unwrap()
            .clone();
        let original = facts
            .cfg_nodes
            .iter()
            .find(|n| n.kind == types::CfgNodeKind::Return)
            .unwrap();
        let mut cfg = crate::CfgResult::default();
        // Isolate the product bound using distinct lowered CFG occurrences of
        // the same written return, without a large parser workload.
        for i in 0..=crate::mode::LAZY_MAX_NODES_PER_UNIT {
            let mut exit = original.clone();
            exit.id = types::CfgNodeId::generate(&exit.function_id, "return_occurrence", i as u32);
            cfg.normal_exit_sources.push(exit.id);
            cfg.nodes.push(exit);
        }
        let ts_lang = frontend.parser.tree_sitter_language();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&ts_lang).unwrap();
        let tree = parser.parse(source, None).unwrap();
        let ctx = ExtractionCtx {
            ts_lang: &ts_lang,
            root: tree.root_node(),
            source,
            file_id,
            file_path: Path::new("bounded.cpp"),
            language: Language::Cpp,
        };
        let mut nodes = vec![parameter.clone()];
        let mut diagnostics = vec![];
        assert_eq!(
            append_parameter_outputs(&ctx, &cfg, &mut nodes, &mut diagnostics, &()),
            Some(true)
        );
        assert_eq!(nodes.len(), crate::mode::LAZY_MAX_NODES_PER_UNIT + 1);
        assert_eq!(diagnostics.len(), 1);
        assert!(
            diagnostics[0]
                .message
                .starts_with("reference_exit_budget_exceeded:")
        );
        struct Cancel;
        impl crate::CancelCheck for Cancel {
            fn is_cancelled(&self) -> bool {
                true
            }
        }
        assert!(
            append_parameter_outputs(&ctx, &cfg, &mut vec![parameter], &mut vec![], &Cancel)
                .is_none()
        );
    }

    #[test]
    fn reference_definitions_keep_their_separate_return_exits() {
        let source = "bool fill(int& output, bool choose) { if (choose) { output = 7; return true; } output = 9; return false; }\n";
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            FileId::generate("returns.cpp"),
            Path::new("returns.cpp"),
            source,
            "test",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();
        let exits: Vec<_> = facts
            .data_nodes
            .iter()
            .filter(|n| n.kind == DataNodeKind::ParameterOutput)
            .collect();
        assert_eq!(
            exits.len(),
            2,
            "each recorded normal return keeps its definitions"
        );
        for (returned, write) in [
            ("return true;", "output = 7"),
            ("return false;", "output = 9"),
        ] {
            let exit = exits
                .iter()
                .find(|n| {
                    source[n.range.start_byte as usize..n.range.end_byte as usize] == *returned
                })
                .unwrap();
            let sources: Vec<_> = facts
                .dataflow_edges
                .iter()
                .filter(|e| e.target == exit.id && e.kind == DataFlowKind::Assign)
                .map(|e| facts.data_nodes.iter().find(|n| n.id == e.source).unwrap())
                .collect();
            assert_eq!(sources.len(), 1, "{returned}: {sources:?}");
            assert_eq!(
                sources[0].range.start_byte as usize,
                source.find(write).unwrap()
            );
        }
    }

    #[test]
    fn reference_output_uses_normal_exit_definitions_in_full_extraction() {
        let source =
            "void store(int& output, int first, int last) { output = first; output = last; }\n";
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            FileId::generate("out.cpp"),
            Path::new("out.cpp"),
            source,
            "test",
            ExtractionMode::Full,
            &(),
        )
        .unwrap();
        let exits: Vec<_> = facts
            .data_nodes
            .iter()
            .filter(|n| n.kind == DataNodeKind::ParameterOutput)
            .collect();
        assert_eq!(exits.len(), 1);
        let exit = exits[0];
        assert_eq!(exit.arg_index, Some(0));
        let sources: Vec<_> = facts
            .dataflow_edges
            .iter()
            .filter(|e| e.target == exit.id && e.kind == DataFlowKind::Assign)
            .map(|e| facts.data_nodes.iter().find(|n| n.id == e.source).unwrap())
            .collect();
        assert_eq!(sources.len(), 1, "{sources:#?}");
        assert_eq!(
            sources[0].range.start_byte as usize,
            source.rfind("output = last").unwrap()
        );
        assert_eq!(sources[0].binding_id, exit.binding_id);
    }
}
