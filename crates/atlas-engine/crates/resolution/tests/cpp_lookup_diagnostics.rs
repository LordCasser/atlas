use std::{path::Path, sync::Arc};

use db::Store;
use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use resolution::ReferenceResolver;
use types::{FileId, Language, ReferenceKind, cpp::CppTypeLookupFailureKind as Kind};

const CALLER: &str = r#"
#include "types.hpp"
int app::Owner::invoke() { return value->run(); }
"#;

fn declarations(base: &str) -> String {
    format!(
        r#"
namespace api {{ {base} }}
namespace app {{
template<class T> struct Handle {{ T* operator->() const {{ return nullptr; }} }};
struct Device {{ int run() {{ return 1; }} }};
struct Owner : api::Base {{ Handle<Device> value; int invoke(); }};
}}
"#
    )
}

fn insert(store: &Store, path: &str, source: &str) {
    let facts = extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        FileId::generate(path),
        Path::new(path),
        source,
        "fixture",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    // Several assays replace this file between passes. Match the indexer's
    // atomic replacement, including invalidation of callers of old symbols.
    store
        .replace_file_facts_with_invalidation(&FileId::generate(path), &facts)
        .unwrap();
}

fn setup(header: &str) -> Arc<Store> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    insert(&store, "types.hpp", header);
    insert(&store, "caller.cpp", CALLER);
    store
}

#[test]
fn typedef_declarator_names_preserve_unrelated_members_and_scalar_argument_identity() {
    for parallel in [false, true] {
        for name in ["uint64_t", "Counter"] {
            let store = Arc::new(Store::open_in_memory().unwrap());
            store.init_schema().unwrap();
            insert(
                &store,
                "types.hpp",
                &format!(
                    "typedef unsigned long long {name}; namespace api {{ struct Reader {{ int read() {{ return 1; }} }}; void consume({name}); void pointer({name}*); }}"
                ),
            );
            insert(
                &store,
                "caller.cpp",
                &format!(
                    "#include \"types.hpp\"\nnamespace api {{ void caller(Reader& obj, {name} id) {{ obj.read(); consume(id); pointer(id); }} }}"
                ),
            );
            let mut resolver = ReferenceResolver::new(store.clone());
            let (resolved, stats) = if parallel {
                resolver
                    .resolve_all_parallel(store.clone(), None, None)
                    .unwrap()
            } else {
                resolver
                    .resolve_for_files(&[
                        FileId::generate("types.hpp"),
                        FileId::generate("caller.cpp"),
                    ])
                    .unwrap()
            };
            assert!(stats.warnings.is_empty());
            for (call, expected) in [
                ("read", Some("api::Reader::read")),
                ("consume", Some("api::consume")),
                ("pointer", None),
            ] {
                let targets: Vec<_> = resolved
                    .iter()
                    .filter(|(r, _)| {
                        r.file_id == FileId::generate("caller.cpp")
                            && r.kind == ReferenceKind::Call
                            && r.name == call
                    })
                    .map(|(_, t)| t.symbol_id)
                    .collect();
                if let Some(expected) = expected {
                    let symbol = store
                        .find_symbols_by_qname(expected)
                        .unwrap()
                        .pop()
                        .unwrap();
                    assert_eq!(targets, vec![symbol.id], "{name}: {call}");
                } else {
                    assert!(
                        targets.is_empty(),
                        "{name}: scalar must not match a pointer parameter"
                    );
                }
            }
            assert!(
                store
                    .cpp_type_lookup_failures(&|| false)
                    .unwrap()
                    .is_empty(),
                "{name}"
            );
        }
    }
}

#[test]
fn argument_template_identity_preserves_stops_and_clears_supported_cases() {
    for parallel in [false, true] {
        for (definition, supported) in [
            ("", false),
            ("template<class T> struct Envelope {};", true),
            ("template<class T> struct Envelope : T {};", false),
        ] {
            let store = Arc::new(Store::open_in_memory().unwrap());
            store.init_schema().unwrap();
            let header = format!(
                "namespace api {{ struct Tag {{}}; {definition} void consume(int, Envelope<Tag>); void known(int); void pointer(int*); void missing(Absent, Envelope<Tag>); }}"
            );
            let caller = r#"
#include "types.hpp"
namespace api {
void direct(int id, Envelope<Tag> value) { consume(id, value); known(id); pointer(id); }
void closure(int id, Envelope<Tag> value) { auto task = [id, value] { consume(id, value); }; }
void earlier(Absent id, Envelope<Tag> value) { missing(id, value); }
}
"#;
            insert(&store, "types.hpp", &header);
            insert(&store, "caller.cpp", caller);
            let mut resolver = ReferenceResolver::new(store.clone());
            let (resolved, stats) = if parallel {
                resolver
                    .resolve_all_parallel(store.clone(), None, None)
                    .unwrap()
            } else {
                resolver
                    .resolve_for_files(&[
                        FileId::generate("types.hpp"),
                        FileId::generate("caller.cpp"),
                    ])
                    .unwrap()
            };
            assert!(stats.warnings.is_empty());
            let calls: Vec<_> = store
                .find_references_by_file(&FileId::generate("caller.cpp"))
                .unwrap()
                .into_iter()
                .filter(|r| r.kind == ReferenceKind::Call)
                .collect();
            let failures = store.cpp_type_lookup_failures(&|| false).unwrap();
            let consume: Vec<_> = calls.iter().filter(|r| r.name == "consume").collect();
            assert_eq!(consume.len(), 2);
            for call in consume {
                if supported {
                    let target = resolved
                        .iter()
                        .find(|(r, _)| r.id == call.id)
                        .unwrap()
                        .1
                        .symbol_id;
                    assert_eq!(
                        store
                            .find_symbol_by_id(&target)
                            .unwrap()
                            .unwrap()
                            .qualified_name,
                        "api::consume"
                    );
                    assert!(!failures.contains_key(&call.id));
                    continue;
                }
                assert!(!resolved.iter().any(|(r, _)| r.id == call.id));
                let failure = failures
                    .get(&call.id)
                    .expect("retain the reached argument type stop");
                assert_eq!(failure.kind, Kind::TemplateUnsupported);
                assert_eq!(failure.name, "Envelope");
                assert_eq!(failure.scope, "api");
                assert_eq!(failure.file_id, FileId::generate("caller.cpp"));
                assert_eq!(
                    &caller[failure.range.start_byte as usize..failure.range.end_byte as usize],
                    "Envelope<Tag> value"
                );
                assert!(failure.related_declarations.is_empty());
            }
            let missing = calls.iter().find(|r| r.name == "missing").unwrap();
            let failure = failures.get(&missing.id).unwrap();
            assert_eq!(failure.kind, Kind::DefinitionUnavailable);
            assert_eq!(failure.name, "Absent");
            let known = calls.iter().find(|r| r.name == "known").unwrap();
            let target = resolved
                .iter()
                .find(|(r, _)| r.id == known.id)
                .unwrap()
                .1
                .symbol_id;
            assert_eq!(
                store
                    .find_symbol_by_id(&target)
                    .unwrap()
                    .unwrap()
                    .qualified_name,
                "api::known"
            );
            let pointer = calls.iter().find(|r| r.name == "pointer").unwrap();
            assert!(!resolved.iter().any(|(r, _)| r.id == pointer.id));
            assert!(!failures.contains_key(&pointer.id));
        }
    }
}

#[test]
fn argument_type_identity_budget_is_an_unexamined_remainder() {
    let parameters = (0..65)
        .map(|i| format!("class T{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let arguments = vec!["int"; 65].join(",");
    let source = format!(
        "template<{parameters}> struct Wide {{}}; void consume(Wide<{arguments}>) {{}} void known() {{}} void entry(Wide<{arguments}> value) {{ consume(value); known(); }}"
    );
    for parallel in [false, true] {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        insert(&store, "main.cpp", &source);
        let mut resolver = ReferenceResolver::new(store.clone());
        let (resolved, _) = if parallel {
            resolver
                .resolve_all_parallel(store.clone(), None, None)
                .unwrap()
        } else {
            resolver
                .resolve_for_files(&[FileId::generate("main.cpp")])
                .unwrap()
        };
        let calls = store
            .find_references_by_file(&FileId::generate("main.cpp"))
            .unwrap();
        let consume = calls
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.name == "consume")
            .unwrap();
        assert!(!resolved.iter().any(|(r, _)| r.id == consume.id));
        let failures = store.cpp_type_lookup_failures(&|| false).unwrap();
        let failure = &failures[&consume.id];
        assert_eq!(failure.kind, Kind::AnalysisBudgetExceeded);
        assert_eq!(failure.code(), "cpp_type_analysis_budget_exceeded");
        assert!(failure.message().contains("unexamined remainder"));
        assert_eq!(
            &source[failure.range.start_byte as usize..failure.range.end_byte as usize],
            format!("Wide<{arguments}> value")
        );
        let known = calls
            .iter()
            .find(|r| r.kind == ReferenceKind::Call && r.name == "known")
            .unwrap();
        assert!(resolved.iter().any(|(r, _)| r.id == known.id));
    }
}

fn run(store: &Arc<Store>, parallel: bool) -> (types::ReferenceUse, Option<types::SymbolId>) {
    let call = store
        .find_references_by_file(&FileId::generate("caller.cpp"))
        .unwrap()
        .into_iter()
        .find(|r| r.kind == ReferenceKind::Call && r.name == "run")
        .unwrap();
    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, stats) = if parallel {
        resolver
            .resolve_all_parallel(store.clone(), None, None)
            .unwrap()
    } else {
        resolver
            .resolve_for_files(&[
                FileId::generate("types.hpp"),
                FileId::generate("caller.cpp"),
            ])
            .unwrap()
    };
    assert!(stats.warnings.is_empty(), "{:?}", stats.warnings);
    let target = resolved
        .into_iter()
        .find(|(r, t)| r.id == call.id && t.strategy != types::ResolutionStrategy::ImplicitOperator)
        .map(|(_, t)| t.symbol_id);
    (call, target)
}

#[test]
fn missing_inherited_type_lookup_is_located_in_both_project_resolution_paths() {
    let header = declarations("struct Base;");
    for parallel in [false, true] {
        let store = setup(&header);
        let (call, target) = run(&store, parallel);
        assert!(target.is_none());
        let failures = store.cpp_type_lookup_failures(&|| false).unwrap();
        let failure = &failures[&call.id];
        assert_eq!(failure.kind, Kind::DefinitionUnavailable);
        assert_eq!(failure.name, "api::Base");
        assert_eq!(failure.scope, "app");
        assert_eq!(failure.file_id, FileId::generate("types.hpp"));
        assert_eq!(
            &header[failure.range.start_byte as usize..failure.range.end_byte as usize],
            "api::Base"
        );
        assert!(failure.message().contains("omitted extraction"));
    }
}

#[test]
fn template_base_member_lookup_locates_its_stop_with_or_without_a_definition() {
    for parallel in [false, true] {
        for template in ["", "template<class T> struct Layer : T {};"] {
            let header = format!(
                r#"
namespace api {{ struct Tag {{ int run() {{ return 2; }} }}; {template} }}
namespace app {{
struct Device : api::Layer<api::Tag> {{
    int implicit() {{ return run(); }}
    int explicit_this() {{ return this->run(); }}
}};
struct Owner {{ Device* value; int invoke(); }};
}}
"#
            );
            let store = setup(&header);
            let (_, target) = run(&store, parallel);
            assert!(target.is_none());
            let failures = store.cpp_type_lookup_failures(&|| false).unwrap();
            let calls: Vec<_> = ["types.hpp", "caller.cpp"]
                .into_iter()
                .flat_map(|file| {
                    store
                        .find_references_by_file(&FileId::generate(file))
                        .unwrap()
                })
                .filter(|r| r.kind == ReferenceKind::Call && r.name == "run")
                .collect();
            assert_eq!(calls.len(), 3);
            for call in calls {
                let failure = failures
                    .get(&call.id)
                    .expect("locate the actual template-base stop");
                assert_eq!(failure.kind, Kind::TemplateUnsupported);
                assert_eq!(failure.name, "api::Layer");
                assert_eq!(failure.scope, "app");
                assert_eq!(failure.file_id, FileId::generate("types.hpp"));
                assert_eq!(
                    &header[failure.range.start_byte as usize..failure.range.end_byte as usize],
                    "api::Layer<api::Tag>"
                );
                assert!(failure.related_declarations.is_empty());
            }
            // Supported ordinary inheritance produces a useful target and
            // replaces the failed lookup. A missing template definition was
            // never established by the earlier, pre-lookup stop.
            insert(
                &store,
                "types.hpp",
                &header.replace("api::Layer<api::Tag>", "api::Tag"),
            );
            let (call, target) = run(&store, parallel);
            let target = store.find_symbol_by_id(&target.unwrap()).unwrap().unwrap();
            assert_eq!(target.qualified_name, "api::Tag::run");
            assert!(
                !store
                    .cpp_type_lookup_failures(&|| false)
                    .unwrap()
                    .contains_key(&call.id)
            );
        }
    }
}

#[test]
fn later_success_and_unknown_replace_the_previous_diagnostic() {
    for parallel in [false, true] {
        let store = setup(&declarations("struct Base;"));
        let (call, _) = run(&store, parallel);
        assert!(
            store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .contains_key(&call.id)
        );
        insert(&store, "types.hpp", &declarations("struct Base {};"));
        let (_, target) = run(&store, parallel);
        let target = target.expect("complete independent inputs must produce a useful call target");
        assert!(
            !store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .contains_key(&call.id)
        );
        assert_eq!(
            store
                .get_reference_by_id(call.id.as_bytes())
                .unwrap()
                .unwrap()
                .resolved
                .unwrap()
                .symbol_id,
            target
        );

        store.invalidate_all_references().unwrap();
        insert(&store, "types.hpp", &declarations("struct Base;"));
        let (_, target) = run(&store, parallel);
        assert!(target.is_none());
        assert_eq!(
            store.cpp_type_lookup_failures(&|| false).unwrap()[&call.id].kind,
            Kind::DefinitionUnavailable
        );

        // Unsupported pointer qualifications stop before name lookup. Reusing the
        // previous missing-base explanation would misstate the actual attempt.
        insert(
            &store,
            "types.hpp",
            &declarations("struct Base;")
                .replace("Handle<Device> value", "volatile Handle<Device> value"),
        );
        let (_, target) = run(&store, parallel);
        assert!(target.is_none());
        assert!(
            !store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .contains_key(&call.id)
        );
    }
}

#[test]
fn ambiguous_and_unmodeled_names_are_not_reported_as_missing_definitions() {
    for parallel in [false, true] {
        for (base, expected) in [
            ("struct Base {}; struct Base {};", Kind::DefinitionAmbiguous),
            ("using Base = void (*)();", Kind::NameHidingUnsupported),
            // The inherited non-template Handle hides the outer template. Lookup
            // now finds that declaration, but Handle<Device> cannot denote it.
            (
                "struct Base { struct Handle {}; };",
                Kind::TemplateUnsupported,
            ),
        ] {
            let store = setup(&declarations(base));
            let (call, target) = run(&store, parallel);
            assert!(target.is_none());
            assert_eq!(
                store.cpp_type_lookup_failures(&|| false).unwrap()[&call.id].kind,
                expected,
                "{base}"
            );
        }
        // A supported alias to a fundamental type cannot be a base record.
        // It no longer fails alias binding, and must not retain that old cause
        // or claim a missing type definition. Non-record bases remain a general
        // unresolved call until that failure has a dedicated diagnostic.
        let store = setup(&declarations("using Base = int;"));
        let (call, target) = run(&store, parallel);
        assert!(target.is_none());
        assert!(
            !store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .contains_key(&call.id)
        );
    }
}

#[test]
fn out_of_include_scope_definition_does_not_erase_a_missing_prerequisite() {
    for parallel in [false, true] {
        let store = setup(&declarations("struct Base;"));
        insert(&store, "unrelated.hpp", "namespace api { struct Base {}; }");
        let (call, target) = run(&store, parallel);
        assert!(target.is_none());
        assert_eq!(
            store.cpp_type_lookup_failures(&|| false).unwrap()[&call.id].kind,
            Kind::DefinitionUnavailable
        );
    }
}

fn member_declarations(base: &str, own_member: bool) -> String {
    let method = if own_member {
        "int run() { return 1; }"
    } else {
        ""
    };
    format!(
        r#"
namespace api {{ {base} }}
namespace noise {{ struct Other {{ int run() {{ return 99; }} }}; }}
namespace app {{
struct Device : api::Base {{
    {method}
    int implicit() {{ return run(); }}
    int via_this() {{ return this->run(); }}
}};
struct Owner {{ Device* value; int invoke(); }};
}}
"#
    )
}

#[test]
fn member_and_nonvirtual_lookup_preserve_missing_base_causes_and_clear_them_after_repair() {
    for parallel in [false, true] {
        for own_member in [false, true] {
            let header = member_declarations("struct Base;", own_member);
            let store = setup(&header);
            let (call, target) = run(&store, parallel);
            assert!(target.is_none());
            let failures = store.cpp_type_lookup_failures(&|| false).unwrap();
            let failed_calls: Vec<_> = ["types.hpp", "caller.cpp"]
                .into_iter()
                .flat_map(|file| {
                    store
                        .find_references_by_file(&FileId::generate(file))
                        .unwrap()
                })
                .filter(|r| r.kind == ReferenceKind::Call && r.name == "run")
                .collect();
            assert_eq!(failed_calls.len(), 3, "object, implicit and this calls");
            for reference in failed_calls {
                let failure = &failures[&reference.id];
                assert_eq!(failure.kind, Kind::DefinitionUnavailable);
                assert_eq!(failure.name, "api::Base");
                assert_eq!(failure.scope, "app");
                assert_eq!(failure.file_id, FileId::generate("types.hpp"));
                assert_eq!(
                    &header[failure.range.start_byte as usize..failure.range.end_byte as usize],
                    "api::Base"
                );
            }
            insert(
                &store,
                "types.hpp",
                &member_declarations("struct Base { int run() { return 2; } };", own_member),
            );
            let (_, target) = run(&store, parallel);
            let target = store.find_symbol_by_id(&target.unwrap()).unwrap().unwrap();
            assert_eq!(
                target.qualified_name,
                if own_member {
                    "app::Device::run"
                } else {
                    "api::Base::run"
                }
            );
            assert!(
                !store
                    .cpp_type_lookup_failures(&|| false)
                    .unwrap()
                    .contains_key(&call.id)
            );

            // A known virtual changes the reason for withholding the body.
            // It must not retain the old missing-base diagnostic or select noise.
            insert(
                &store,
                "types.hpp",
                &member_declarations(
                    "struct Base { virtual int run() { return 2; } };",
                    own_member,
                ),
            );
            let (_, target) = run(&store, parallel);
            assert!(target.is_none());
            let failures = store.cpp_type_lookup_failures(&|| false).unwrap();
            let failure = failures.get(&call.id).unwrap_or_else(|| {
                panic!("parallel={parallel} own_member={own_member}: {failures:?}")
            });
            assert_eq!(failure.kind, Kind::VirtualMemberUnresolved);
            assert_eq!(failure.name, "run");
            assert_eq!(failure.scope, "api::Base");
            assert_eq!(failure.file_id, call.file_id);
            assert_eq!(failure.range, call.range);
            let [(file, range)] = failure.related_declarations.as_slice() else {
                panic!("missing virtual declaration: {failure:?}");
            };
            assert_eq!(*file, FileId::generate("types.hpp"));
            let current = member_declarations(
                "struct Base { virtual int run() { return 2; } };",
                own_member,
            );
            assert!(current[range.start_byte as usize..range.end_byte as usize].contains("run"));
        }
    }
}

#[test]
fn virtual_member_diagnostic_keeps_overload_selection_and_other_failures_distinct() {
    let header = r#"
namespace api {
struct Base { virtual int run() { return 1; } };
struct Derived : Base { int run() { return 2; } };
struct Mixed { virtual int run(double); int run(int); };
struct Plain { virtual int noise(); int run() { return 3; } };
struct Missing;
}
"#;
    let caller = r#"
#include "types.hpp"
int base(api::Base& value) { return value.run(); }
int derived(api::Derived& value) { return value.run(); }
int mixed(api::Mixed& value) { return value.run(1); }
int plain(api::Plain& value) { return value.run(); }
int missing(api::Missing& value) { return value.run(); }
"#;
    for parallel in [false, true] {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        insert(&store, "types.hpp", header);
        insert(&store, "caller.cpp", caller);
        let mut resolver = ReferenceResolver::new(store.clone());
        let (resolved, _) = if parallel {
            resolver
                .resolve_all_parallel(store.clone(), None, None)
                .unwrap()
        } else {
            resolver
                .resolve_for_files(&[
                    FileId::generate("types.hpp"),
                    FileId::generate("caller.cpp"),
                ])
                .unwrap()
        };
        let failures = store.cpp_type_lookup_failures(&|| false).unwrap();
        let calls: Vec<_> = store
            .find_references_by_file(&FileId::generate("caller.cpp"))
            .unwrap()
            .into_iter()
            .filter(|r| r.kind == ReferenceKind::Call && r.name == "run")
            .collect();
        assert_eq!(calls.len(), 5);
        for call in calls {
            let owner = store
                .find_symbol_by_id(&call.source_symbol.unwrap())
                .unwrap()
                .unwrap();
            let target = resolved.iter().find(|(r, _)| r.id == call.id);
            if owner.name == "plain" {
                let target = store
                    .find_symbol_by_id(&target.unwrap().1.symbol_id)
                    .unwrap()
                    .unwrap();
                assert_eq!(target.qualified_name, "api::Plain::run");
                assert!(!failures.contains_key(&call.id));
                continue;
            }
            assert!(target.is_none());
            let failure = &failures[&call.id];
            if owner.name == "missing" {
                assert_eq!(failure.kind, Kind::DefinitionUnavailable);
                continue;
            }
            assert_eq!(failure.kind, Kind::VirtualMemberUnresolved);
            assert_eq!(failure.range, call.range);
            assert_eq!(failure.file_id, call.file_id);
            assert_eq!(failure.related_declarations.len(), 1);
            let (file, range) = failure.related_declarations[0];
            assert_eq!(file, FileId::generate("types.hpp"));
            let declaration = &header[range.start_byte as usize..range.end_byte as usize];
            assert!(declaration.contains("run"));
            assert_eq!(
                failure.scope,
                if owner.name == "mixed" {
                    "api::Mixed"
                } else {
                    "api::Base"
                }
            );
            // The mixed case would select the ordinary int overload in valid
            // C++. This diagnostic must not claim that the virtual overload
            // was selected; it records the resolver's reached limitation.
            assert!(
                failure
                    .message()
                    .contains("not proof of overload selection")
            );
        }
    }
}

#[test]
fn a_successful_template_base_absence_proof_does_not_publish_the_ordinary_lookup_failure() {
    let header = member_declarations(
        "struct Tag {}; template<class T> struct Layer : T {}; struct Base : Layer<Tag> {};",
        true,
    );
    for parallel in [false, true] {
        let store = setup(&header);
        let (call, target) = run(&store, parallel);
        assert!(target.is_some());
        assert!(
            !store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .contains_key(&call.id)
        );
    }
}

#[test]
fn type_lookup_restrictions_retain_the_declarations_that_triggered_them() {
    let base = declarations("struct Base {};");
    for parallel in [false, true] {
        for (origin, expected) in [
            (
                "namespace extra {} namespace app { using namespace extra; }",
                "using namespace extra;",
            ),
            (
                "namespace extra { template<class T> struct Handle {}; } namespace app { using extra::Handle; }",
                "using extra::Handle;",
            ),
        ] {
            let header = format!("#include \"origins.hpp\"\n{base}");
            let store = setup(&header);
            insert(&store, "origins.hpp", origin);
            // A similarly scoped but non-included file is not a cause.
            insert(
                &store,
                "unrelated.hpp",
                "namespace app { using namespace missing; }",
            );
            let (call, target) = run(&store, parallel);
            assert!(target.is_none());
            let failure = store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .remove(&call.id)
                .unwrap();
            assert_eq!(failure.kind, Kind::LookupRestricted);
            let json = serde_json::to_value(&failure).unwrap();
            let locations: Vec<(FileId, types::TextRange)> = serde_json::from_value(
                json["related_declarations"].clone(),
            )
            .expect("reached lookup restrictions must retain original declaration locations");
            assert_eq!(locations.len(), 1, "{failure:?}");
            assert_eq!(locations[0].0, FileId::generate("origins.hpp"));
            let range = locations[0].1;
            assert_eq!(
                &origin[range.start_byte as usize..range.end_byte as usize],
                expected
            );
            // Removing the actually included declaration clears its old cause
            // and restores useful resolution despite the unrelated file.
            insert(&store, "origins.hpp", "namespace extra {}");
            let (_, target) = run(&store, parallel);
            assert!(target.is_some());
            assert!(
                !store
                    .cpp_type_lookup_failures(&|| false)
                    .unwrap()
                    .contains_key(&call.id)
            );
        }
    }
}

#[test]
fn local_type_lookup_origin_respects_declaration_order_and_block_scope() {
    let header = "namespace extra {} namespace app { struct Device { int run() { return 1; } }; }";
    for (body, expected) in [
        (
            "using namespace extra; app::Device value; return value.run();",
            true,
        ),
        (
            "app::Device value; using namespace extra; return value.run();",
            false,
        ),
        (
            "{ using namespace extra; } app::Device value; return value.run();",
            false,
        ),
        (
            "using namespace extra; ::app::Device value; return value.run();",
            false,
        ),
    ] {
        let store = setup(header);
        let caller = format!("#include \"types.hpp\"\nint entry() {{ {body} }}");
        insert(&store, "caller.cpp", &caller);
        let (call, target) = run(&store, true);
        if expected {
            assert!(target.is_none());
            let failure = store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .remove(&call.id)
                .unwrap();
            assert_eq!(failure.kind, Kind::LookupRestricted);
            assert_eq!(failure.related_declarations.len(), 1);
            let (file, range) = failure.related_declarations[0];
            assert_eq!(file, FileId::generate("caller.cpp"));
            assert_eq!(
                &caller[range.start_byte as usize..range.end_byte as usize],
                "using namespace extra;"
            );
        } else {
            assert!(target.is_some(), "{body}");
            assert!(
                !store
                    .cpp_type_lookup_failures(&|| false)
                    .unwrap()
                    .contains_key(&call.id)
            );
        }
    }
}

#[test]
fn direct_name_lookup_reports_only_reached_visible_declarations_and_clears_old_causes() {
    for parallel in [false, true] {
        for origin in [
            "namespace extra {} namespace app { using namespace extra; }",
            "namespace extra { int run(); } namespace app { using extra::run; }",
        ] {
            let store = setup(
                "#include \"origins.hpp\"\nnamespace app { struct Owner { int invoke(); }; }",
            );
            insert(&store, "origins.hpp", origin);
            insert(
                &store,
                "unrelated.hpp",
                "namespace app { using namespace absent; }",
            );
            let caller = "#include \"types.hpp\"\nint run() { return 1; }\nint app::Owner::invoke() { return run(); }\n";
            insert(&store, "caller.cpp", caller);
            let (call, target) = run(&store, parallel);
            assert!(target.is_none());
            let failure = store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .remove(&call.id)
                .expect("a reached direct name restriction needs its original declaration");
            assert_eq!(failure.code(), "cpp_name_lookup_restricted");
            assert_eq!(failure.scope, "app");
            assert_eq!(failure.name, "run");
            assert_eq!((failure.file_id, failure.range), (call.file_id, call.range));
            assert_eq!(failure.related_declarations.len(), 1);
            let (file, range) = failure.related_declarations[0];
            assert_eq!(file, FileId::generate("origins.hpp"));
            let expected = if origin.contains("using namespace") {
                "using namespace extra;"
            } else {
                "using extra::run;"
            };
            assert_eq!(
                &origin[range.start_byte as usize..range.end_byte as usize],
                expected
            );
            insert(&store, "origins.hpp", "namespace extra {}");
            let (_, target) = run(&store, parallel);
            assert!(target.is_some());
            assert!(
                !store
                    .cpp_type_lookup_failures(&|| false)
                    .unwrap()
                    .contains_key(&call.id)
            );
        }
    }
}

#[test]
fn direct_local_lookup_diagnostic_respects_scope_order_and_absolute_names() {
    for parallel in [false, true] {
        for (body, restricted) in [
            ("using namespace extra; return run();", true),
            ("run(); using namespace extra; return 0;", false),
            ("{ using namespace extra; } return run();", false),
            ("using namespace extra; return ::run();", false),
        ] {
            let store = setup("namespace extra {}");
            let caller = format!(
                "#include \"types.hpp\"\nint run() {{ return 1; }}\nint entry() {{ {body} }}\n"
            );
            insert(&store, "caller.cpp", &caller);
            let (call, target) = run(&store, parallel);
            let failure = store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .remove(&call.id);
            if restricted {
                assert!(target.is_none());
                let failure = failure.expect("a local import must have a readable cause");
                assert_eq!(failure.code(), "cpp_name_lookup_restricted");
                assert_eq!(failure.related_declarations.len(), 1);
                let (file, range) = failure.related_declarations[0];
                assert_eq!(file, call.file_id);
                assert_eq!(
                    &caller[range.start_byte as usize..range.end_byte as usize],
                    "using namespace extra;"
                );
            } else {
                assert!(target.is_some(), "{body}");
                assert!(failure.is_none(), "{failure:?}");
            }
        }
    }
}

#[test]
fn class_suffix_lookup_restriction_locates_the_written_suffix_without_bypassing_members() {
    for parallel in [false, true] {
        let header = "#define NOTE(x)\nnamespace app { struct Owner { void declared() NOTE(lock); int invoke(); }; }\n";
        let store = setup(header);
        insert(
            &store,
            "caller.cpp",
            "#include \"types.hpp\"\nint run() { return 1; }\nint app::Owner::invoke() { return run(); }\n",
        );
        let (call, target) = run(&store, parallel);
        assert!(target.is_none());
        let failure = store
            .cpp_type_lookup_failures(&|| false)
            .unwrap()
            .remove(&call.id)
            .expect("an unmodeled class suffix must identify its lookup restriction");
        assert_eq!(failure.code(), "cpp_name_lookup_restricted");
        assert_eq!(failure.scope, "app::Owner");
        assert_eq!(failure.related_declarations.len(), 1);
        let (file, range) = failure.related_declarations[0];
        assert_eq!(file, FileId::generate("types.hpp"));
        assert_eq!(
            &header[range.start_byte as usize..range.end_byte as usize],
            "NOTE(lock);"
        );
        insert(&store, "types.hpp", &header.replace(" NOTE(lock)", ""));
        let (_, target) = run(&store, parallel);
        assert!(target.is_some());
        assert!(
            !store
                .cpp_type_lookup_failures(&|| false)
                .unwrap()
                .contains_key(&call.id)
        );
    }
}
