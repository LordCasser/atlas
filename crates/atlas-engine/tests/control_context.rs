#![cfg(feature = "cpp")]
use atlas_engine::{
    ExtractionMode, IndexPipeline, IndexPipelineOptions, NoopSink, Store,
    call_context::inspect_call_context,
};
use std::sync::Arc;

fn inspect(source: &str, expression: &str) -> (Vec<(String, String)>, Vec<String>) {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("sample.cpp"), source).unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    IndexPipeline::new(
        store.clone(),
        root.path().into(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .run(&NoopSink, &mut || false)
    .unwrap();
    let calls_before = store.get_all_call_references().unwrap();
    let edges_before = store.get_all_edges().unwrap();
    let start = source.rfind(expression).unwrap() as u32;
    let result = inspect_call_context(
        &store,
        root.path(),
        "sample.cpp",
        start,
        start + expression.len() as u32,
        true,
        &|| false,
    )
    .unwrap();
    assert_eq!(calls_before, store.get_all_call_references().unwrap());
    assert_eq!(edges_before, store.get_all_edges().unwrap());
    let controls = result
        .items
        .into_iter()
        .filter(|i| i.role.starts_with("control_"))
        .map(|i| {
            assert!(i.symbol_id.is_none());
            assert!(!i.related_locations.is_empty());
            (
                i.role.into(),
                source[i.location.range.start_byte as usize..i.location.range.end_byte as usize]
                    .to_string(),
            )
        })
        .collect();
    (
        controls,
        result.gaps.into_iter().map(|g| g.code.into()).collect(),
    )
}

#[test]
fn builtin_logical_rhs_has_a_guard_from_both_operand_types() {
    for (op, role) in [
        ("&&", "control_true_branch"),
        ("||", "control_false_branch"),
        ("and", "control_true_branch"),
        ("or", "control_false_branch"),
    ] {
        let source = format!(
            "bool first(); bool second(); bool run(bool outer) {{ if (!outer) return false; return !first() {op} !second(); }}"
        );
        assert_eq!(
            inspect(&source, "second()").0,
            vec![
                ("control_false_branch".into(), "(!outer)".into()),
                (role.into(), "!first()".into())
            ],
            "{source}"
        );
        for selection in [
            "first()",
            "return !first()",
            "return !first() && !second();",
        ] {
            if source.contains(selection) {
                assert_eq!(
                    inspect(&source, selection).0,
                    vec![("control_false_branch".into(), "(!outer)".into())]
                );
            }
        }
    }
    let source = "bool run(bool first, int second) { return first && second; }";
    assert_eq!(
        inspect(source, "second").0,
        vec![("control_true_branch".into(), "first".into())]
    );
}

#[test]
fn logical_overloads_and_unknown_types_do_not_establish_short_circuiting() {
    for declarations in [
        "struct Check {}; Check first(); Check second(); bool operator&&(Check, Check);",
        "struct Check {}; bool first(); Check second(); bool operator&&(bool, Check);",
        "enum Check { yes }; Check first(); Check second(); bool operator&&(Check, Check);",
        "bool first();",
    ] {
        let source = format!(
            "{declarations} bool run(bool outer) {{ if (!outer) return false; return first() && second(); }}"
        );
        let (controls, gaps) = inspect(&source, "second()");
        assert_eq!(
            controls,
            vec![("control_false_branch".into(), "(!outer)".into())],
            "{source}"
        );
        assert!(
            gaps.contains(&"control_short_circuit_unestablished".into()),
            "{source}: {gaps:?}"
        );
    }
    let shadowed = "struct Check {}; bool operator&&(Check, Check); bool run(bool first) { { Check first; Check second; return first && second; } }";
    assert!(inspect(shadowed, "second").0.is_empty());
    let macro_operand = "#define NULL make_check()\nstruct Check {}; Check make_check(); bool second(); bool operator&&(Check, bool); bool run() { return NULL && second(); }";
    let (conditions, gaps) = inspect(macro_operand, "second()");
    assert!(conditions.is_empty());
    assert!(gaps.contains(&"control_short_circuit_unestablished".into()));
}

#[test]
fn logical_operand_budget_preserves_outer_guards() {
    let source = format!(
        "bool first(); bool second(); bool run(bool outer) {{ if (!outer) return false; return {}first() && second(); }}",
        "!".repeat(257)
    );
    let (conditions, gaps) = inspect(&source, "second()");
    assert_eq!(
        conditions,
        vec![("control_false_branch".into(), "(!outer)".into())]
    );
    assert!(gaps.contains(&"control_analysis_budget".into()));
}

#[test]
fn logical_guards_compose_without_crossing_evaluation_boundaries() {
    let source = "bool first(); bool second(); bool last(); bool run(bool choose) { return choose ? (first() || (second() && last())) : false; }";
    let mut guards = inspect(source, "last()").0;
    guards.sort();
    assert_eq!(
        guards,
        vec![
            ("control_false_branch".into(), "first()".into()),
            ("control_true_branch".into(), "choose".into()),
            ("control_true_branch".into(), "second()".into())
        ]
    );
    for expression in [
        "sizeof(first() && second())",
        "noexcept(first() && second())",
        "requires { first() && second(); }",
    ] {
        let source =
            format!("bool first(); bool second(); void run() {{ auto v = {expression}; }}");
        assert!(inspect(&source, "second()").0.is_empty(), "{source}");
    }
    let source = "bool first(); bool second(); void run() { auto pending = [saved = first() && second()] { return 7; }; }";
    assert_eq!(
        inspect(source, "second()").0,
        vec![("control_true_branch".into(), "first()".into())]
    );
    assert!(inspect(source, "7").0.is_empty());
    let source = "bool first(); bool second(); void run() { auto value = first() && second(); int after = 7; }";
    assert!(inspect(source, "7").0.is_empty());
}

#[test]
fn capture_initializer_conditions_belong_to_creation_and_not_the_body() {
    let source = "void run(bool enabled, int input) { if (!enabled) return; auto pending = [saved = input + 1] { int body_value = 7; }; }";
    assert_eq!(
        inspect(source, "input + 1").0,
        vec![("control_false_branch".into(), "(!enabled)".into())]
    );
    assert!(
        inspect(source, "7").0.is_empty(),
        "the closure body does not inherit the creation branch"
    );
}

#[test]
fn early_return_and_nested_branches_preserve_actual_boolean_paths() {
    let source = "int submit(); int denied(); int run(bool allowed) { if (!allowed) { return denied(); } return submit(); }\n";
    let (controls, gaps) = inspect(source, "submit()");
    assert_eq!(
        controls,
        vec![("control_false_branch".into(), "(!allowed)".into())]
    );
    assert!(gaps.contains(&"control_flow_limited".into()));
    assert_eq!(
        inspect(source, "denied()").0,
        vec![("control_true_branch".into(), "(!allowed)".into())]
    );
    assert!(
        inspect(source, "!allowed").0.is_empty(),
        "evaluating a check does not require its outcome"
    );
    let source = "void submit(); void run(bool outer, bool inner) { if (outer) { if (inner) { submit(); } } }\n";
    assert_eq!(
        inspect(source, "submit()").0,
        vec![
            ("control_true_branch".into(), "(outer)".into()),
            ("control_true_branch".into(), "(inner)".into())
        ]
    );
}

#[test]
fn rejoining_and_bypass_paths_cannot_be_misreported_as_guards() {
    for source in [
        "void submit(); void observe(); void run(bool allowed) { if (allowed) { observe(); } submit(); }\n",
        "void submit(); void run(bool allowed, bool retry) { if (!allowed) { if (retry) return; } submit(); }\n",
        "void submit(); void run(bool skip, bool allowed) { if (skip) goto work; if (!allowed) return; work: submit(); }\n",
    ] {
        assert!(inspect(source, "submit()").0.is_empty(), "{source}");
    }
}

#[test]
fn conditional_operands_have_source_guards_without_asserting_the_condition_value() {
    let source = "int left(); int right(); int run(bool allowed, bool choose) { if (!allowed) return 0; int value = choose ? left() : right(); return value; }";
    for (selected, role) in [
        ("left()", "control_true_branch"),
        ("right()", "control_false_branch"),
    ] {
        let (conditions, gaps) = inspect(source, selected);
        assert_eq!(
            conditions,
            vec![
                ("control_false_branch".into(), "(!allowed)".into()),
                (role.into(), "choose".into())
            ]
        );
        assert!(!gaps.contains(&"control_expression_unestablished".into()));
    }
    for selected in ["choose ?", "choose ? left() : right()", "return value;"] {
        assert_eq!(
            inspect(source, selected).0,
            vec![("control_false_branch".into(), "(!allowed)".into())]
        );
    }
    let nested = "int left(); int right(); int run(bool outer, bool inner) { return outer ? (inner ? left() : right()) : 0; }";
    let mut conditions = inspect(nested, "right()").0;
    conditions.sort();
    assert_eq!(
        conditions,
        vec![
            ("control_false_branch".into(), "inner".into()),
            ("control_true_branch".into(), "outer".into())
        ]
    );
}

#[test]
fn conditional_guards_respect_evaluation_scope_and_syntax_recovery() {
    for expression in [
        "sizeof(choose ? left() : right())",
        "noexcept(choose ? left() : right())",
        "requires { choose ? left() : right(); }",
    ] {
        let source = format!(
            "int left(); int right(); void run(bool choose) {{ auto value = {expression}; }}"
        );
        assert!(inspect(&source, "left()").0.is_empty(), "{source}");
    }
    let source = "int left(); int right(); void run(bool choose) { auto pending = [saved = choose ? left() : right()] { return 7; }; }";
    assert_eq!(
        inspect(source, "left()").0,
        vec![("control_true_branch".into(), "choose".into())]
    );
    assert!(inspect(source, "7").0.is_empty());
    let recovered =
        "int left(); int right(); int run(bool choose) { return (choose + ) ? left() : right(); }";
    assert!(inspect(recovered, "left()").0.is_empty());
}

#[test]
fn conditional_operand_guards_preserve_independent_guards_and_scope() {
    for expression in [
        "choose ? left() : right()",
        "(choose ? (left()) : (right()))",
    ] {
        let source = format!(
            "int left(); int right(); int run(bool allowed, bool choose) {{ if (!allowed) return 0; int value = {expression}; return value; }}"
        );
        for selected in ["left()", "right()"] {
            let (conditions, gaps) = inspect(&source, selected);
            assert_eq!(
                conditions,
                vec![
                    ("control_false_branch".into(), "(!allowed)".into()),
                    (
                        if selected == "left()" {
                            "control_true_branch"
                        } else {
                            "control_false_branch"
                        }
                        .into(),
                        "choose".into()
                    )
                ]
            );
            assert!(!gaps.contains(&"control_expression_unestablished".into()));
        }
        for selected in ["choose ?", "return value;"] {
            assert!(
                !inspect(&source, selected)
                    .1
                    .contains(&"control_expression_unestablished".into())
            );
        }
    }
    let source =
        "void work(); void run(bool choose) { auto callback = choose ? [] { work(); } : [] {}; }";
    let (conditions, gaps) = inspect(source, "work()");
    assert!(
        conditions.is_empty(),
        "a lambda body has its own execution context"
    );
    assert!(!gaps.contains(&"control_expression_unestablished".into()));
}

#[test]
fn loop_limits_and_callback_execution_contexts_are_explicit() {
    let source = "void submit(); void run(bool allowed) { while (allowed) { submit(); } }\n";
    // The positive loop condition remains a measured 0/1 in the experiment.
    // This assertion only verifies that the consumer receives its located limit.
    let (conditions, gaps) = inspect(source, "submit()");
    assert!(conditions.is_empty());
    assert!(gaps.contains(&"control_loop_condition_unestablished".into()));
    let source = "void submit(); void run(bool allowed) { if (allowed) { auto callback = []() { submit(); }; } }\n";
    assert!(
        inspect(source, "submit()").0.is_empty(),
        "creating a lambda under a branch does not carry that branch into an invocation"
    );
}

#[test]
fn an_unresolved_call_can_still_have_source_control_conditions() {
    let source = "void run(bool allowed) { if (!allowed) return; unavailable(); }\n";
    let (controls, gaps) = inspect(source, "unavailable()");
    assert_eq!(
        controls,
        vec![("control_false_branch".into(), "(!allowed)".into())]
    );
    assert!(gaps.contains(&"control_flow_limited".into()));
}

#[test]
fn unresolved_preprocessing_cannot_turn_variant_paths_into_required_guards() {
    let source = "void submit(); void run(bool allowed) {\n#ifdef MODE\nif (!allowed) return;\n#endif\nsubmit();\n}\n";
    let (conditions, gaps) = inspect(source, "submit()");
    assert!(conditions.is_empty());
    assert!(gaps.contains(&"control_configuration_unestablished".into()));
}

#[test]
fn statement_variants_preserve_common_runtime_guards() {
    for body in [
        "#ifdef MODE\nobserve();\n#else\nother();\n#endif\nif (allowed) submit();",
        "#if FIRST\nobserve();\n#elif SECOND\nother();\n#else\nobserve();\n#endif\nif (allowed) submit();",
        "if (allowed) submit();\n#ifndef MODE\nobserve();\n#endif",
        "#if MODE\nif (allowed) submit();\n#else\nobserve();\n#endif",
        "#ifdef MODE\n#ifdef NESTED\nobserve();\n#endif\n#else\nother();\n#endif\nif (allowed) submit();",
    ] {
        let source = format!(
            "void submit(); void observe(); void other(); void run(bool allowed) {{\n{body}\n}}\n"
        );
        let (conditions, gaps) = inspect(&source, "submit()");
        assert_eq!(
            conditions,
            vec![("control_true_branch".into(), "(allowed)".into())],
            "{source}: {gaps:?}"
        );
        assert!(gaps.contains(&"control_configuration_unestablished".into()));
    }
}

#[test]
fn configuration_alternatives_keep_forward_and_backward_bypasses() {
    for body in [
        "#ifdef MODE\ngoto work;\n#endif\nif (!allowed) return;\nwork: submit();",
        "#if FIRST\nobserve();\n#elif SECOND\ngoto work;\n#else\nobserve();\n#endif\nif (!allowed) return;\nwork: submit();",
        "if (allowed) { work: submit(); }\n#ifdef MODE\ngoto work;\n#endif",
        "#if MODE\nif (!allowed) return;\n#endif\nsubmit();",
    ] {
        let source =
            format!("void submit(); void observe(); void run(bool allowed) {{\n{body}\n}}\n");
        let (conditions, gaps) = inspect(&source, "submit()");
        assert!(conditions.is_empty(), "{source}: {conditions:?}");
        assert!(gaps.contains(&"control_configuration_unestablished".into()));
    }
}

#[test]
fn opaque_configuration_content_cannot_establish_a_later_guard() {
    for body in [
        "#ifdef MODE\n#include \"unknown.inc\"\n#endif",
        "#ifdef MODE\n#ifdef INNER\n#include \"unknown.inc\"\n#endif\n#endif",
    ] {
        let source = format!(
            "void submit(); void run(bool allowed) {{\n{body}\nif (allowed) submit();\n}}\n"
        );
        let (conditions, gaps) = inspect(&source, "submit()");
        assert!(conditions.is_empty(), "{source}: {conditions:?}");
        assert!(gaps.contains(&"control_configuration_unestablished".into()));
    }
}

#[test]
fn variant_labels_do_not_turn_unresolved_jumps_into_exclusion() {
    let source = "void submit(); void observe(); void run(bool allowed) {\ngoto work;\n#if MODE\nwork: return;\n#else\nif (allowed) { work: submit(); }\n#endif\n}\n";
    let (conditions, gaps) = inspect(source, "submit()");
    assert!(conditions.is_empty(), "{conditions:?}");
    assert!(
        !gaps.contains(&"control_reachability_unestablished".into()),
        "{gaps:?}"
    );
    assert!(gaps.contains(&"control_configuration_unestablished".into()));
    let source = "void submit(); void run(bool allowed) { goto missing; if (allowed) submit(); }";
    let (conditions, gaps) = inspect(source, "submit()");
    assert!(conditions.is_empty());
    assert!(
        gaps.contains(&"control_transfer_unestablished".into()),
        "{gaps:?}"
    );
}

#[test]
fn independent_guards_survive_unknown_regions_without_selecting_a_variant() {
    for source in [
        "void submit(); void run(bool allowed, bool check) { if (allowed) {\n#ifdef MODE\nif (!check) return;\n#endif\nsubmit();\n} }\n",
        "void submit(); void run(bool allowed, bool check) { if (allowed) {\n#if MODE\nif (!check) return;\n#else\nobserve();\n#endif\nsubmit();\n} }\n",
        "void submit(); void run(bool allowed, bool check) { if (allowed) { while (check) { observe(); } submit(); } }\n",
    ] {
        let (conditions, gaps) = inspect(source, "submit()");
        assert_eq!(
            conditions,
            vec![("control_true_branch".into(), "(allowed)".into())],
            "{source}"
        );
        assert!(gaps.iter().any(|g| matches!(
            g.as_str(),
            "control_configuration_unestablished" | "control_loop_condition_unestablished"
        )));
    }
}

#[test]
fn unknown_regions_cannot_establish_dependent_guards_or_hide_possible_bypasses() {
    for source in [
        "void submit(); void run(bool allowed) {\n#ifdef MODE\ngoto work;\n#endif\nif (!allowed) return;\nwork: submit();\n}\n",
        "void submit(); void run(bool allowed, bool check) { while (check) observe(); if (!allowed) return; submit(); }\n",
        "void submit(); void run(bool allowed) {\n#ifdef MODE\nif (!allowed) return;\n#else\nsubmit();\n#endif\n}\n",
    ] {
        let (conditions, gaps) = inspect(source, "submit()");
        assert!(conditions.is_empty(), "{source}: {conditions:?}");
        assert!(gaps.iter().any(|g| matches!(
            g.as_str(),
            "control_configuration_unestablished" | "control_loop_condition_unestablished"
        )));
    }
}
