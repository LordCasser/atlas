#![cfg(feature = "cpp")]

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
};
use types::{DataNodeKind, FileFacts, FileId, Language};

fn extract(source: &str, language: Language) -> FileFacts {
    let path = if language == Language::Cpp {
        "flow.cpp"
    } else {
        "flow.ts"
    };
    extract_file_with_mode(
        &create_frontend(language).unwrap(),
        FileId::generate(path),
        Path::new(path),
        source,
        "test",
        ExtractionMode::Full,
        &(),
    )
    .unwrap()
}

fn origins(facts: &FileFacts, source: &str, marker: &str) -> BTreeSet<String> {
    let start = (source.find(marker).unwrap() + "return ".len()) as u32;
    let sink = facts
        .data_nodes
        .iter()
        .find(|n| n.kind == DataNodeKind::Return && n.range.start_byte == start)
        .unwrap();
    let mut pending = vec![sink.id];
    let mut seen = HashSet::new();
    let nodes: HashMap<_, _> = facts.data_nodes.iter().map(|n| (n.id, n)).collect();
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        pending.extend(
            facts
                .dataflow_edges
                .iter()
                .filter(|e| e.target == id)
                .map(|e| e.source),
        );
    }
    seen.iter()
        .filter_map(|id| nodes.get(id))
        .filter(|n| matches!(n.kind, DataNodeKind::Parameter | DataNodeKind::Literal))
        .map(|n| source[n.range.start_byte as usize..n.range.end_byte as usize].to_string())
        .collect()
}

fn assert_origins(source: &str, marker: &str, expected: &[&str]) -> FileFacts {
    let facts = extract(source, Language::Cpp);
    assert_eq!(
        origins(&facts, source, marker),
        expected.iter().map(|s| s.to_string()).collect(),
        "{source}\n{:?}",
        facts.diagnostics
    );
    facts
}

#[test]
fn unconditional_overwrite_removes_old_definition() {
    let facts = assert_origins(
        "int f(int input) { int value = input; value = 7; return value; }",
        "return value",
        &["7"],
    );
    assert!(facts.diagnostics.is_empty(), "{:?}", facts.diagnostics);
}

#[test]
fn weak_update_result_does_not_feed_its_own_operand_without_a_loop() {
    for (source, loop_carried) in [
        (
            "int f(int seed) { auto total = seed; total++; return total; }",
            false,
        ),
        (
            "int f(int seed, int count) { auto total = seed; while (count--) { total++; } return total; }",
            true,
        ),
    ] {
        let facts = extract(source, Language::Cpp);
        let start = source.find("total++").unwrap() as u32;
        let mutation = facts
            .data_nodes
            .iter()
            .find(|n| n.kind == DataNodeKind::Local && n.range.start_byte == start)
            .unwrap();
        let operand = facts
            .data_nodes
            .iter()
            .find(|n| n.kind == DataNodeKind::VariableUse && n.range.start_byte == start)
            .unwrap();
        assert_eq!(mutation.binding_id, operand.binding_id);
        assert_eq!(
            facts
                .dataflow_edges
                .iter()
                .any(|e| e.source == mutation.id && e.target == operand.id),
            loop_carried,
            "only a preceding loop iteration can feed this mutation's operand"
        );
        assert!(origins(&facts, source, "return total").contains("seed"));
        assert!(
            facts
                .diagnostics
                .iter()
                .any(|d| d.message.starts_with("use_def_write_order_unmodeled:")),
            "operand order does not establish overwrite effects for auto"
        );
    }
}

#[test]
fn branches_keep_only_definitions_that_can_reach_the_join() {
    assert_origins(
        "int f(int first, int second, bool choose) { int value = first; if (choose) value = second; return value; }",
        "return value",
        &["first", "second"],
    );
    assert_origins(
        "int f(int first, int second, int third, bool choose) { int value = first; if (choose) value = second; else value = third; return value; }",
        "return value",
        &["second", "third"],
    );
}

#[test]
fn returning_branch_does_not_pollute_the_other_branch() {
    assert_origins(
        "int f(int first, int second, bool choose) { int value = first; if (choose) { value = second; return 0; } return value; }",
        "return value",
        &["first"],
    );
}

#[test]
fn loop_backedge_preserves_prior_iteration_value() {
    let source = "int f(int first, int second, int count) { int value = first; while (count-- > 0) { int previous = value; value = second; if (count == 0) return previous; } return value; }";
    assert_origins(source, "return previous", &["first", "second"]);
    assert_origins(source, "return value", &["first", "second"]);
}

#[test]
fn break_and_continue_follow_their_actual_cfg_successors() {
    assert_origins(
        "int f(int first, int second, int third, bool choose) { int value = first; while (choose) { value = second; break; value = third; } return value; }",
        "return value",
        &["first", "second"],
    );
    assert_origins(
        "int f(int first, int second, int third, int count) { int value = first; while (count-- > 0) { value = second; continue; value = third; } return value; }",
        "return value",
        &["first", "second"],
    );
}

#[test]
fn direct_goto_keeps_jump_target_and_excludes_disconnected_write() {
    assert_origins(
        "int f(int first, int second) { int value = first; goto done; value = second; done: return value; }",
        "return value",
        &["first"],
    );
    assert_origins(
        "int f(int first, int second) { int value = first; goto assign; return 0; assign: value = second; return value; }",
        "return value",
        &["second"],
    );
}

#[test]
fn lazy_function_window_preserves_loop_origins_without_other_function_data() {
    use types::lazy::{AnalysisUnit, LazyWindow};
    let source = "int f(int first, int second, int count) { int value = first; while (count-- > 0) { int previous = value; value = second; if (count == 0) return previous; } return value; } int other(int unrelated) { return unrelated; }";
    let full = extract(source, Language::Cpp);
    let function = full.symbols.iter().find(|s| s.name == "f").unwrap();
    let file = FileId::generate("flow.cpp");
    let unit = AnalysisUnit::from_function(file, function.id, function.range);
    let window = LazyWindow {
        seed_unit: unit.clone(),
        units: vec![unit],
        variable_focus: None,
        truncated: false,
        units_built: 0,
        units_cached: 0,
        units_pending: 0,
        pending_job_ids: vec![],
        quality: None,
        capability_mask: Default::default(),
    };
    let lazy = extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        file,
        Path::new("flow.cpp"),
        source,
        "test",
        ExtractionMode::LazyDataflow {
            include_parameter_outputs: true,
            window,
            callsites: full.callsites.clone(),
        },
        &(),
    )
    .unwrap();
    assert_eq!(
        origins(&lazy, source, "return previous"),
        BTreeSet::from(["first".into(), "second".into()])
    );
    assert!(
        lazy.data_nodes
            .iter()
            .all(|n| n.function_id == Some(function.id))
    );
    assert!(!lazy.budget_exceeded);
}

#[test]
fn rhs_reads_previous_value_before_replacing_it() {
    let source = "int f(int input) { int value = input; value = value + 1; return value; }";
    let facts = assert_origins(source, "return value", &["input", "1"]);
    let write = facts
        .data_nodes
        .iter()
        .find(|n| {
            n.kind == DataNodeKind::Local
                && n.range.start_byte == source.find("value = value").unwrap() as u32
        })
        .unwrap();
    let read = facts
        .data_nodes
        .iter()
        .find(|n| {
            n.kind == DataNodeKind::VariableUse
                && n.range.start_byte == source.find("value + 1").unwrap() as u32
        })
        .unwrap();
    assert!(
        !facts
            .dataflow_edges
            .iter()
            .any(|e| e.source == write.id && e.target == read.id)
    );
}

#[test]
fn lexical_shadow_and_uninvoked_closure_do_not_replace_outer_local() {
    assert_origins(
        "int f(int first, int second) { int value = first; { int value = second; value = 7; } return value; }",
        "return value",
        &["first"],
    );
    assert_origins(
        "int f(int first, int second) { int value = first; auto pending = [&]() { value = second; }; return value; }",
        "return value",
        &["first"],
    );
}

#[test]
fn conditional_expression_writes_keep_old_value_and_report_order_limit() {
    for expression in [
        "choose && (value = second)",
        "choose ? (value = second) : 0",
    ] {
        let source = format!(
            "int f(int first, int second, bool choose) {{ int value = first; {expression}; return value; }}"
        );
        let facts = assert_origins(&source, "return value", &["first", "second"]);
        let write_start = source.find("value = second").unwrap() as u32;
        assert!(
            facts
                .diagnostics
                .iter()
                .any(|d| d.message.starts_with("use_def_write_order_unmodeled:")
                    && d.range.is_some_and(|r| r.start_byte == write_start))
        );
    }
}

#[test]
fn cpp_object_assignment_does_not_prove_the_old_value_was_overwritten() {
    let source = "struct Box { int number; Box& operator=(const Box&) { return *this; } }; Box f(Box first, Box second) { Box value = first; value = second; return value; }";
    let facts = assert_origins(source, "return value", &["first", "second"]);
    let assignment = source.find("value = second").unwrap() as u32;
    assert!(
        facts
            .diagnostics
            .iter()
            .any(|d| d.message.starts_with("use_def_write_order_unmodeled:")
                && d.range.is_some_and(|r| r.start_byte == assignment))
    );
}

#[cfg(feature = "typescript")]
#[test]
fn typescript_uses_the_same_cfg_definition_rules() {
    for (body, expected) in [
        ("let value = first; value = 7; return value;", vec!["7"]),
        (
            "let value = first; if (choose) value = second; return value;",
            vec!["first", "second"],
        ),
        (
            "let value = first; while (count-- > 0) { let previous = value; value = second; if (count === 0) return previous; } return value;",
            vec!["first", "second"],
        ),
    ] {
        let source = format!(
            "function f(first: number, second: number, choose: boolean, count: number) {{ {body} }}"
        );
        let facts = extract(&source, Language::TypeScript);
        let marker = if body.contains("return previous") {
            "return previous"
        } else {
            "return value"
        };
        assert_eq!(
            origins(&facts, &source, marker),
            expected.into_iter().map(str::to_string).collect(),
            "{source}"
        );
    }
}
