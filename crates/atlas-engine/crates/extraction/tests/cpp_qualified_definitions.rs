#![cfg(feature = "cpp")]

use std::path::Path;

use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use types::{FileFacts, FileId, Language, SymbolKind};

fn extract(source: &str, mode: ExtractionMode) -> FileFacts {
    extract_file_with_mode(
        &create_frontend(Language::Cpp).unwrap(),
        FileId::generate("qualified.cpp"),
        Path::new("qualified.cpp"),
        source,
        "fixture",
        mode,
        &(),
    )
    .unwrap()
}

#[test]
fn typedef_names_follow_declarator_roles_without_accepting_type_keywords() {
    for (source, expected) in [
        (
            "typedef unsigned long long uint64_t;",
            vec![("uint64_t", Some("unsigned long long"))],
        ),
        (
            "typedef unsigned long long Count;",
            vec![("Count", Some("unsigned long long"))],
        ),
        (
            "typedef int intptr_t, Token;",
            vec![("intptr_t", Some("int")), ("Token", Some("int"))],
        ),
        ("typedef int *intptr_t;", vec![("intptr_t", None)]),
        ("typedef int (*intptr_t)(int);", vec![("intptr_t", None)]),
        ("typedef int int;", vec![]),
        ("typedef int bool;", vec![]),
        ("typedef int short;", vec![]),
    ] {
        let facts = extract(source, ExtractionMode::Structural);
        let names: Vec<_> = facts
            .symbols
            .iter()
            .filter(|s| s.kind == SymbolKind::TypeAlias)
            .collect();
        assert_eq!(names.len(), expected.len(), "{source}");
        let cpp = facts.cpp_types.as_ref().unwrap();
        for (name, target) in expected {
            let symbol = names
                .iter()
                .find(|s| s.name == name)
                .unwrap_or_else(|| panic!("{source}: {name}"));
            assert_eq!(
                &source[symbol.name_range.start_byte as usize..symbol.name_range.end_byte as usize],
                name
            );
            let alias = cpp
                .aliases
                .iter()
                .find(|a| a.symbol_id == symbol.id)
                .unwrap();
            assert_eq!(
                alias.target.as_ref().map(|t| t.name.as_str()),
                target,
                "{source}"
            );
        }
        if !names.is_empty() {
            assert!(
                cpp.lookup_limits.iter().all(|limit| limit.name.is_some()),
                "{source}"
            );
        }
    }
}

#[test]
fn member_macro_inventory_preserves_names_and_rejects_unknown_expansion() {
    use std::collections::HashMap;
    let cases = [
        (
            "#define COPY(C) C(const C&) = delete; C& operator=(const C&) = delete\n#define MOVE(C) C(C&&) = delete; C& operator=(C&&) = delete\n#define MEMBERS(C) COPY(C); MOVE(C)\n",
            "Base",
            Some(vec!["Base", "operator="]),
        ),
        (
            "#define MEMBERS(C) C(); ~C();\n",
            "Base",
            Some(vec!["Base", "~Base"]),
        ),
        (
            "#define INNER(X) int X;\n#define MEMBERS(X) INNER(X)\n",
            "generated",
            Some(vec!["generated"]),
        ),
        (
            "#define FIELD(X) private: int X ## _;\n#define GET(X) public: int get ## X() const { return X ## _; }\n#define MEMBERS(X) FIELD(X) GET(X)\n",
            "value",
            Some(vec!["getvalue", "value_"]),
        ),
        (
            "#define FIELD(X) int X ## _\n#define GET(X) int get ## X() { return 0; }\n#define MEMBERS(X) FIELD(X) GET(X)\n",
            "value",
            None,
        ),
        (
            "#define FIELD(X) int X ## _\n#define MEMBERS(X) FIELD(X); FIELD(other)\n",
            "value",
            Some(vec!["other_", "value_"]),
        ),
        (
            "#define MEMBERS(X) NEXT(X)\n#define NEXT(X) MEMBERS(X)\n",
            "field",
            None,
        ),
        (
            "#define INNER(X) int\n#define MEMBERS(X) INNER(X) X\n",
            "field",
            None,
        ),
        (
            "#define INNER(X) int X;\n#define MEMBERS(X) INNER(X)\n#undef INNER\n",
            "field",
            None,
        ),
        ("#define MEMBERS(X)\n", "unused", Some(vec![])),
        (
            "#if FIRST\n#define MEMBERS(TEXT) int first;\n#else\n#define MEMBERS(TEXT) int second;\n#endif\n",
            "u\"choice\"",
            Some(vec!["first", "second"]),
        ),
        (
            "#define MEMBERS(TEXT) static constexpr const char16_t *label_ = TEXT; static const char16_t* label() { return label_; }\n",
            "u\"a,b\"",
            Some(vec!["label", "label_"]),
        ),
        (
            "#define MEMBERS(NAME) int NAME;\n",
            "Handle",
            Some(vec!["Handle"]),
        ),
        (
            "#define MEMBERS(NAME) int field; int NAME() { return 0; }\n",
            "read",
            Some(vec!["field", "read"]),
        ),
        (
            "#define MEMBERS(NAME) int NAME;\n#define read changed\n",
            "read",
            None,
        ),
        (
            "#define MEMBERS(NAME) int field;\n#undef MEMBERS\n",
            "read",
            None,
        ),
        (
            "#define MEMBERS(NAME) int NAME ## suffix;\n",
            "read",
            Some(vec!["readsuffix"]),
        ),
        (
            "#define MEMBERS(NAME) int prefix ## NAME ## suffix;\n",
            "read",
            Some(vec!["prefixreadsuffix"]),
        ),
        ("#define MEMBERS(NAME) int NAME ## suffix;\n", "", None),
        ("#define MEMBERS(NAME) int NAME ## suffix;\n", "1", None),
        (
            "#define MEMBERS(NAME) int NAME ## suffix;\n",
            "two tokens",
            None,
        ),
        (
            "#define MEMBERS(NAME) int NAME ## suffix;\n#define readsuffix changed\n",
            "read",
            None,
        ),
        ("#define MEMBERS(NAME) int NAME ##;\n", "read", None),
        ("#define MEMBERS(NAME) int ## NAME;\n", "read", None),
        ("#define MEMBERS(NAME) using NAME = int;\n", "Handle", None),
        ("#define MEMBERS(NAME) friend void NAME();\n", "read", None),
        (
            "#define MEMBERS(NAME) static Box<::support::Token> NAME();\n",
            "make",
            Some(vec!["make"]),
        ),
        (
            "#define MEMBERS(NAME) static Box< ::support::Token> NAME();\n",
            "make",
            Some(vec!["make"]),
        ),
        (
            // The raw grammar loses the macro definition around this comment.
            // Tokenization cannot repair missing definition facts; retain unknown.
            "#define MEMBERS(NAME) static Box</* gap */::support::Token> NAME();\n",
            "make",
            None,
        ),
        (
            "#define MEMBERS(NAME) static Box<::> NAME();\n",
            "make",
            None,
        ),
        (
            "#define MEMBERS(NAME) static Box<:::Token> NAME();\n",
            "make",
            None,
        ),
        ("#define MEMBERS(NAME) int NAME<:2:>;\n", "array", None),
        (
            "#define MEMBERS(NAME) public: int NAME;\n",
            "read",
            Some(vec!["read"]),
        ),
        (
            "#define MEMBERS(C) public: static C& instance(); C(const C&) = delete; private: int hidden_; protected: virtual ~C() = default; public:\n",
            "Base",
            Some(vec!["Base", "hidden_", "instance", "~Base"]),
        ),
        (
            "#define MEMBERS(NAME) private: int NAME; protected: int other_;\n",
            "read",
            Some(vec!["other_", "read"]),
        ),
        (
            "#define private public: int hidden; private\n#define MEMBERS(NAME) private: int NAME;\n",
            "read",
            None,
        ),
        ("#define MEMBERS(NAME) enum { NAME };\n", "read", None),
        (
            "#define MEMBERS(NAME) Unknown* operator->();\n",
            "read",
            None,
        ),
        ("#define MEMBERS(...) int field;\n", "read", None),
        ("#define MEMBERS(NAME) int NAME;\n", "a,b", None),
        ("", "u\"unknown\"", None),
    ];
    for (macros, argument, expected) in cases {
        let source = format!(
            "{macros}namespace demo {{ struct Base {{ public: MEMBERS({argument}); int ordinary() {{ return 1; }} }}; }}"
        );
        let facts = extract(&source, ExtractionMode::Structural);
        let cpp = facts.cpp_types.unwrap();
        assert_eq!(cpp.member_macros.len(), 1, "{source}");
        let site = &cpp.member_macros[0];
        assert_eq!(site.scope, "demo::Base");
        assert_eq!(
            &source[site.range.start_byte as usize..site.range.end_byte as usize],
            site.text
        );
        assert!(
            cpp.lookup_limits
                .iter()
                .any(|limit| limit.name.is_none() && limit.declaration_range == site.range)
        );
        let mut definitions = HashMap::<_, Vec<_>>::new();
        for definition in &cpp.macros {
            definitions
                .entry(definition.name.as_str())
                .or_default()
                .push(definition);
        }
        let names = extraction::cpp_member_macros::introduced_names(site, &definitions);
        assert_eq!(
            names,
            expected.map(|names| names.into_iter().map(String::from).collect()),
            "{source}"
        );
        assert!(
            cpp.records.iter().any(|record| record.is_definition),
            "the raw class identity remains available: {source}"
        );
        // Raw recovery is no longer exempted from the class error check merely
        // because it resembles a macro. The project preparation pipeline must
        // validate actual definitions and reparse before admitting that lookup.
    }
}

#[test]
fn member_macro_call_without_semicolon_requires_self_terminated_replacement() {
    use std::collections::HashMap;
    use types::{TextRange, cpp::CppMemberMacro};
    for (replacement, expected) in [
        (
            "public: I ## NAME(); virtual ~I ## NAME(); static int get ## NAME() { return 7; }",
            Some(vec!["IDevice", "getDevice", "~IDevice"]),
        ),
        ("int value;", Some(vec!["value"])),
        ("int value", None),
        (
            "int a ## NAME; int NAME ## b;",
            Some(vec!["Deviceb", "aDevice"]),
        ),
        ("int a ## ## NAME;", None),
        ("int a ## 1;", None),
        ("const char* value = # NAME;", Some(vec!["value"])),
    ] {
        let source = format!("#define MEMBERS(NAME) {replacement}\n");
        let facts = extract(&source, ExtractionMode::Structural);
        let cpp = facts.cpp_types.unwrap();
        let mut definitions = HashMap::<_, Vec<_>>::new();
        for definition in &cpp.macros {
            definitions
                .entry(definition.name.as_str())
                .or_default()
                .push(definition);
        }
        let site = CppMemberMacro {
            scope: "example::IDevice".into(),
            name: "MEMBERS".into(),
            text: "MEMBERS(Device)".into(),
            range: TextRange::default(),
        };
        assert_eq!(
            extraction::cpp_member_macros::introduced_names(&site, &definitions),
            expected.map(|names| names.into_iter().map(String::from).collect()),
            "{replacement}"
        );
    }
}

#[test]
fn type_identity_and_member_declarations_have_separate_coverage() {
    for (member, members_known) in [
        (
            "bool different(const Value& other) const { return !this->operator==(other); }",
            true,
        ),
        (
            "bool different(const Value& other) const { return !((*this) == other); }",
            true,
        ),
        ("int broken() { return @; }", true),
        ("int @;", false),
        ("int broken(@);", false),
    ] {
        let source = format!("struct Value {{ bool operator==(const Value&) const; {member} }};");
        for mode in [
            ExtractionMode::Structural,
            ExtractionMode::ResolutionSymbols,
        ] {
            let facts = extract(&source, mode.clone());
            let symbol = facts
                .symbols
                .iter()
                .find(|s| s.qualified_name == "Value")
                .unwrap_or_else(|| panic!("missing type in {mode:?}: {source}"));
            let cpp = facts.cpp_types.as_ref().unwrap();
            let record = cpp
                .records
                .iter()
                .find(|r| r.symbol_id == symbol.id)
                .unwrap_or_else(|| panic!("missing record in {mode:?}: {source}"));
            assert!(record.identity_supported, "{source}");
            assert_eq!(record.lookup_supported, members_known, "{source}");
        }
    }
    let facts = extract(
        "struct Value : @ { int read(); };",
        ExtractionMode::Structural,
    );
    assert!(
        !facts
            .cpp_types
            .unwrap()
            .records
            .iter()
            .any(|r| r.identity_supported)
    );
    let facts = extract(
        "struct Value { int broken() { };",
        ExtractionMode::Structural,
    );
    assert!(
        !facts
            .cpp_types
            .unwrap()
            .records
            .iter()
            .any(|r| r.lookup_supported)
    );
}

#[test]
fn ordinary_constructor_and_primitive_parenthesized_field_are_not_member_macros() {
    let facts = extract(
        "struct Device { Device(); int (value); int read() { return value; } };",
        ExtractionMode::Structural,
    );
    let cpp = facts.cpp_types.unwrap();
    assert!(cpp.member_macros.is_empty());
    assert!(cpp.lookup_limits.is_empty());
    assert!(cpp.records.iter().any(|record| record.lookup_supported));
}

#[test]
fn member_inventory_cannot_clear_errors_when_the_class_structure_is_lost() {
    let source = "#define MEMBERS(TEXT) static const char16_t* label = TEXT;\nnamespace demo { struct Base { public: MEMBERS(u\"literal # and comma,\"); int ordinary() { return 1; } }; }";
    let facts = extract(source, ExtractionMode::Structural);
    let cpp = facts.cpp_types.unwrap();
    assert!(cpp.member_macros.is_empty());
    assert!(!cpp.records.iter().any(|record| record.lookup_supported));
}

#[test]
fn unnamed_pointer_template_parameter_recovery_preserves_other_member_names() {
    for helper in [
        "template<class T, traits::slot<T>* = nullptr> int helper(T) { return 3; }",
        "template<class T, traits::slot<T>* = nullptr> int helper(T);",
        "template<class T, traits::slot<T>* /* default */ = nullptr> int helper(T);",
    ] {
        let source = format!(
            "namespace traits {{ template<class T> using slot = int; }}\nnamespace sample {{ struct Device {{ {helper} int read() {{ return 7; }} }}; }}"
        );
        let facts = extract(&source, ExtractionMode::Structural);
        let device = facts
            .symbols
            .iter()
            .find(|s| s.qualified_name == "sample::Device")
            .unwrap();
        let read = facts
            .symbols
            .iter()
            .find(|s| s.qualified_name == "sample::Device::read")
            .unwrap();
        let cpp = facts.cpp_types.as_ref().unwrap();
        assert!(
            cpp.records
                .iter()
                .any(|r| r.symbol_id == device.id && r.lookup_supported),
            "{source}"
        );
        let limit = cpp
            .lookup_limits
            .iter()
            .find(|l| l.scope == "sample::Device" && l.name.as_deref() == Some("helper"))
            .unwrap();
        assert_eq!(
            &source[limit.declaration_range.start_byte as usize
                ..limit.declaration_range.end_byte as usize],
            helper
        );
        assert!(
            !cpp.lookup_limits
                .iter()
                .any(|l| l.scope == "sample::Device" && l.name.is_none())
        );
        for helper in facts
            .symbols
            .iter()
            .filter(|s| s.qualified_name == "sample::Device::helper")
        {
            assert!(
                !cpp.callables.iter().any(|c| c.symbol_id == helper.id),
                "function-template semantics remain unsupported"
            );
        }
        assert!(cpp.callables.iter().any(|c| c.symbol_id == read.id));
    }
    for helper in [
        "template<class T, traits::slot<T>** = nullptr> int helper(T);",
        "template<class T, traits::slot<T>* = > int helper(T);",
        "template<class T, traits::slot<T>* = nullptr> int helper(T) { return @; }",
        "int helper(traits::slot<int>* = nullptr);",
    ] {
        let source = format!("struct Device {{ {helper} int read() {{ return 7; }} }};");
        let facts = extract(&source, ExtractionMode::Structural);
        assert!(
            !facts
                .cpp_types
                .unwrap()
                .records
                .iter()
                .any(|r| r.lookup_supported),
            "other recoveries retain their limits: {source}"
        );
    }
}

#[test]
fn named_using_limits_only_its_name_even_inside_a_recovered_record() {
    let source = r#"
namespace api {
using external::Device;
class EXPORT Broken : public Missing { using Missing::imported; };
struct Owner : Base { using Base::run; };
}
namespace wildcard { using namespace missing; }
"#;
    let facts = extract(source, ExtractionMode::Structural);
    let cpp = facts.cpp_types.unwrap();
    let names: Vec<_> = cpp
        .lookup_limits
        .iter()
        .filter_map(|limit| {
            limit
                .name
                .as_ref()
                .map(|name| format!("{}::{name}", limit.scope))
        })
        .collect();
    assert_eq!(names, ["api::Device", "api::imported", "api::Owner::run"]);
    let scopes: Vec<_> = cpp
        .lookup_limits
        .iter()
        .filter(|limit| limit.name.is_none())
        .map(|limit| limit.scope.as_str())
        .collect();
    assert_eq!(scopes, ["wildcard"]);
    assert!(
        cpp.lookup_limits
            .iter()
            .all(|limit| limit.block_range.is_none())
    );
}

#[test]
fn local_lookup_limits_keep_declaration_and_verified_block_locations() {
    let source = r#"
namespace api {
struct Device {
    void configure() { using namespace unavailable; }
    void nested() { { using unavailable::helper; } }
};
void outer() { using namespace unavailable; }
class EXPORT Broken { using namespace unavailable; };
}
"#;
    let facts = extract(source, ExtractionMode::Structural);
    let limits = facts.cpp_types.unwrap().lookup_limits;
    assert_eq!(limits.len(), 4);
    for limit in &limits[..3] {
        let block = limit.block_range.expect("ordinary function block");
        assert!(block.start_byte < limit.declaration_range.start_byte);
        assert!(block.end_byte > limit.declaration_range.end_byte);
        let text = &source[limit.declaration_range.start_byte as usize
            ..limit.declaration_range.end_byte as usize];
        assert!(text.starts_with("using "));
    }
    assert_eq!(limits[1].name.as_deref(), Some("helper"));
    assert!(
        limits[3].block_range.is_none(),
        "a recovered class body is not a verified local block"
    );
}

#[test]
fn callable_static_storage_comes_from_its_own_declaration() {
    let source = r#"
struct Device {
    static void declared();
    static void inline_static() {}
    void instance() { static int local = 1; }
};
void Device::declared() {}
static void file_local() {}
void ordinary() { static int local = 1; }
"#;
    let facts = extract(source, ExtractionMode::Structural);
    for name in [
        "declared",
        "inline_static",
        "instance",
        "file_local",
        "ordinary",
    ] {
        let symbols: Vec<_> = facts.symbols.iter().filter(|s| s.name == name).collect();
        assert!(!symbols.is_empty(), "{name}");
        for symbol in symbols {
            let expected = matches!(name, "inline_static" | "file_local")
                || (name == "declared" && symbol.range == symbol.name_range);
            assert_eq!(symbol.static_, expected, "{symbol:?}");
        }
    }
}

#[test]
fn unidentified_template_owner_retains_calls_without_namespace_ownership() {
    let source = r#"
namespace project {
int leaf() { return 1; }
template<class T> struct Box { int run(); };
template<class T> int Box<T>::run() { return leaf(); }
int entry() { return leaf(); }
}
"#;
    let facts = extract(source, ExtractionMode::Structural);
    let entry = facts
        .symbols
        .iter()
        .find(|s| s.qualified_name == "project::entry")
        .unwrap();
    let calls: Vec<_> = facts
        .references
        .iter()
        .filter(|r| r.kind == types::ReferenceKind::Call && r.name == "leaf")
        .collect();
    assert_eq!(calls.len(), 2);
    for call in calls {
        if call.range.start_byte < entry.range.start_byte {
            assert_eq!(
                call.source_symbol, None,
                "an unsupported template owner is not a namespace caller"
            );
        } else {
            assert_eq!(call.source_symbol, Some(entry.id));
        }
    }
}

#[test]
fn recovered_scope_separators_do_not_establish_qualified_definitions() {
    // The input is incomplete and contains an annotation whose definition is
    // unavailable. Recovery must not turn that annotation into a namespace.
    let source = r#"
    explicit PROJECT_API_ATTRIBUTE BufferRangeWrapper(value_t beg, value_t end) NO_EXCEPT
        : begin_at(beg),
          end_at(end) {}
"#;
    for mode in [ExtractionMode::Structural, ExtractionMode::Manifest] {
        let facts = extract(source, mode);
        assert!(
            facts.symbols.iter().all(|symbol| !symbol
                .qualified_name
                .contains("PROJECT_API_ATTRIBUTE::BufferRangeWrapper")),
            "a recovered separator cannot establish a source identity: {:?}",
            facts.symbols
        );
    }

    // A written name remains independently usable when the body is partial.
    let facts = extract(
        "void Project::Range::run() { incomplete + ; }",
        ExtractionMode::Structural,
    );
    assert!(
        facts
            .symbols
            .iter()
            .any(|symbol| symbol.qualified_name == "Project::Range::run")
    );
}

#[test]
fn destructor_definitions_own_body_calls_and_keep_declarations_separate() {
    let source = r#"
void cleanup(int) {}
namespace lifetime {
struct InlineGuard {
    ~InlineGuard() { cleanup(1); }
    void run() { cleanup(2); }
};
struct ExternalGuard { ~ExternalGuard(); };
struct Defaulted { ~Defaulted() = default; };
struct Deleted { ~Deleted() = delete; };
struct Commented { ~ /* spelling */ Commented() { cleanup(5); } };
}
lifetime::ExternalGuard::~ExternalGuard() {
    auto task = [] { cleanup(3); };
    cleanup(4);
}
void construct_only() { lifetime::InlineGuard guard; }
void explicit_destroy(lifetime::InlineGuard* guard) { guard->~InlineGuard(); }
"#;
    let facts = extract(source, ExtractionMode::Structural);
    for (name, count) in [
        ("lifetime::InlineGuard::~InlineGuard", 1),
        ("lifetime::ExternalGuard::~ExternalGuard", 2),
        ("lifetime::Defaulted::~Defaulted", 1),
        ("lifetime::Deleted::~Deleted", 1),
        ("lifetime::Commented::~Commented", 1),
    ] {
        let symbols: Vec<_> = facts
            .symbols
            .iter()
            .filter(|s| s.qualified_name == name)
            .collect();
        assert_eq!(symbols.len(), count, "{name}");
        for symbol in symbols {
            assert!(symbol.name.starts_with('~'));
            let written = &source[symbol.range.start_byte as usize..symbol.range.end_byte as usize];
            let owned: Vec<_> = facts
                .references
                .iter()
                .filter(|r| {
                    r.kind == types::ReferenceKind::Call && r.source_symbol == Some(symbol.id)
                })
                .collect();
            if written.contains("cleanup(") {
                let direct: Vec<_> = owned.iter().filter(|r| r.name == "cleanup").collect();
                // The uninvoked lambda's cleanup belongs to the lambda, not the destructor.
                assert_eq!(direct.len(), 1, "{name}");
                assert!(
                    facts
                        .callsites
                        .iter()
                        .any(|c| { c.caller == symbol.id && c.reference_id == Some(direct[0].id) })
                );
                let callable = facts
                    .cpp_types
                    .as_ref()
                    .unwrap()
                    .callables
                    .iter()
                    .find(|c| c.symbol_id == symbol.id)
                    .unwrap();
                assert!(callable.return_type.is_none());
            } else {
                assert!(owned.is_empty(), "declaration acquired a body: {name}");
                assert!(!written.contains('{'));
            }
        }
    }
    let lambda = facts
        .symbols
        .iter()
        .find(|s| s.name.starts_with("<lambda@"))
        .unwrap();
    assert!(
        facts
            .references
            .iter()
            .any(|r| r.name == "cleanup" && r.source_symbol == Some(lambda.id))
    );
    let construct = facts
        .symbols
        .iter()
        .find(|s| s.name == "construct_only")
        .unwrap();
    assert!(
        !facts
            .references
            .iter()
            .any(|r| r.kind == types::ReferenceKind::Call
                && r.source_symbol == Some(construct.id)
                && r.name.starts_with('~'))
    );
    let explicit = facts
        .symbols
        .iter()
        .find(|s| s.name == "explicit_destroy")
        .unwrap();
    assert!(!facts.symbols.iter().any(|s| s.name.starts_with('~')
        && s.name_range.start_byte > explicit.range.start_byte
        && s.name_range.end_byte < explicit.range.end_byte));
    let manifest = extract(source, ExtractionMode::Manifest);
    assert!(
        manifest
            .symbols
            .iter()
            .any(|s| s.qualified_name == "lifetime::ExternalGuard::~ExternalGuard")
    );
}

#[test]
fn qualified_constructor_definitions_own_initializer_and_body_calls() {
    let source = r#"
int initialize(int value) { return value; }
void touch() {}
namespace alpha {
struct Holder { Holder(int value); Holder(float value); int state; };
}
alpha::Holder::Holder(int value) : state(initialize(value)) { touch(); }
alpha::Holder::Holder(float value) : state(initialize(static_cast<int>(value))) { touch(); }
namespace beta {
struct Holder { Holder(); };
Holder::Holder() { touch(); }
}
int after() { return initialize(4); }
"#;
    let facts = extract(source, ExtractionMode::Structural);
    let constructors: Vec<_> = facts
        .symbols
        .iter()
        .filter(|s| {
            matches!(
                s.qualified_name.as_str(),
                "alpha::Holder::Holder" | "beta::Holder::Holder"
            )
        })
        .collect();
    assert_eq!(constructors.len(), 3);
    assert_ne!(constructors[0].id, constructors[1].id);
    for constructor in constructors {
        let written =
            &source[constructor.range.start_byte as usize..constructor.range.end_byte as usize];
        assert!(written.contains("touch();"));
        assert_eq!(
            &source[constructor.name_range.start_byte as usize
                ..constructor.name_range.end_byte as usize],
            "Holder"
        );
        let owned: Vec<_> = facts
            .references
            .iter()
            .filter(|r| {
                r.kind == types::ReferenceKind::Call
                    && r.source_symbol == Some(constructor.id)
                    && matches!(r.name.as_str(), "initialize" | "touch")
            })
            .collect();
        assert_eq!(
            owned.len(),
            if constructor.qualified_name.starts_with("alpha::") {
                2
            } else {
                1
            }
        );
        for reference in owned {
            assert!(
                reference.range.start_byte >= constructor.range.start_byte
                    && reference.range.end_byte <= constructor.range.end_byte
            );
            assert!(facts.callsites.iter().any(
                |call| call.reference_id == Some(reference.id) && call.caller == constructor.id
            ));
        }
        let callable = facts
            .cpp_types
            .as_ref()
            .unwrap()
            .callables
            .iter()
            .find(|c| c.symbol_id == constructor.id)
            .unwrap();
        assert!(callable.return_type.is_none());
    }
    let after = facts
        .symbols
        .iter()
        .find(|s| s.qualified_name == "after")
        .unwrap();
    assert_eq!(
        facts
            .references
            .iter()
            .filter(|r| r.kind == types::ReferenceKind::Call
                && r.source_symbol == Some(after.id)
                && r.name == "initialize")
            .count(),
        1
    );
    let manifest = extract(source, ExtractionMode::Manifest);
    assert_eq!(
        manifest
            .symbols
            .iter()
            .filter(|s| s.qualified_name == "alpha::Holder::Holder")
            .count(),
        2
    );
    assert!(
        manifest
            .symbols
            .iter()
            .all(|s| s.qualified_name != "beta::Holder::Holder")
    );
}

#[test]
fn qualified_definitions_keep_owner_name_signature_and_body_range() {
    let source = r#"
namespace OHOS {
namespace Notification {
int NotificationStub::OnRemoteRequest(int code) {
    return HandlePublish(code);
}
int NotificationStub::HandlePublish(int code) { return code; }
int OtherStub::HandlePublish(int code) { return -code; }
const Value* NotificationStub::pointer() { return nullptr; }
const Value& NotificationStub::reference() { return value; }
int Nested::Stub::run() { return 1; }
int Spaced /* owner */ :: run() { return 2; }
}
}
int ::Global::run() { return 3; }
"#;
    let facts = extract(source, ExtractionMode::Structural);
    for (qualified, name, signature) in [
        (
            "OHOS::Notification::NotificationStub::OnRemoteRequest",
            "OnRemoteRequest",
            "(int code): int",
        ),
        (
            "OHOS::Notification::NotificationStub::HandlePublish",
            "HandlePublish",
            "(int code): int",
        ),
        (
            "OHOS::Notification::OtherStub::HandlePublish",
            "HandlePublish",
            "(int code): int",
        ),
        (
            "OHOS::Notification::NotificationStub::pointer",
            "pointer",
            "(): const Value",
        ),
        (
            "OHOS::Notification::NotificationStub::reference",
            "reference",
            "(): const Value",
        ),
        ("OHOS::Notification::Nested::Stub::run", "run", "(): int"),
        ("OHOS::Notification::Spaced::run", "run", "(): int"),
        ("Global::run", "run", "(): int"),
    ] {
        let symbols: Vec<_> = facts
            .symbols
            .iter()
            .filter(|s| s.qualified_name == qualified)
            .collect();
        assert_eq!(symbols.len(), 1, "missing or duplicate {qualified}");
        let symbol = symbols[0];
        assert_eq!(symbol.kind, SymbolKind::Function);
        assert_eq!(symbol.name, name);
        assert_eq!(symbol.signature.as_deref(), Some(signature));
        assert_eq!(
            &source[symbol.name_range.start_byte as usize..symbol.name_range.end_byte as usize],
            name
        );
        let body = &source[symbol.range.start_byte as usize..symbol.range.end_byte as usize];
        assert!(body.contains("return "), "{qualified}: {body}");
        assert!(body.ends_with('}'), "{qualified}: {body}");
    }
    let handlers: Vec<_> = facts
        .symbols
        .iter()
        .filter(|s| s.name == "HandlePublish")
        .collect();
    assert_eq!(handlers.len(), 2);
    assert_ne!(
        handlers[0].id, handlers[1].id,
        "different owners must not fold together"
    );
}

#[test]
fn qualified_returns_parameters_and_declarations_are_not_definitions() {
    let source = r#"
Result::Value plain(Arg::Value value) { return value; }
int Stub::declaration(int code);
int Stub::deleted() = delete;
namespace Hidden { int Stub::nested() { return 1; } }
int Outer::Inner::run() { return 2; }
const Value* Stub::pointer() { return nullptr; }
const Value& Stub::reference() { return value; }
"#;
    let structural = extract(source, ExtractionMode::Structural);
    for unexpected in ["Value", "declaration", "deleted"] {
        assert!(
            structural.symbols.iter().all(|s| s.name != unexpected),
            "spurious {unexpected}"
        );
    }
    let manifest = extract(source, ExtractionMode::Manifest);
    let names: Vec<_> = manifest
        .symbols
        .iter()
        .map(|s| s.qualified_name.as_str())
        .collect();
    for expected in [
        "plain",
        "Outer::Inner::run",
        "Stub::pointer",
        "Stub::reference",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    assert!(
        !names.contains(&"Hidden::Stub::nested"),
        "manifest must stay at file scope"
    );
}

#[test]
fn receiver_facts_keep_base_locations_factory_returns_and_auto_initializer_links() {
    let source = r#"
namespace demo {
struct Base {};
struct Device : public virtual Base {};
template<class T> struct Handle {};
struct Derived : Handle<Device>, virtual Base {};
Handle<Device> make();
Handle<Device> const& borrow();
Handle<Device> const* constant();
void entry() { auto device = make(); device->run(); }
}
"#;
    let facts = extract(source, ExtractionMode::Structural);
    let cpp = facts.cpp_types.as_ref().unwrap();
    let symbol = |name: &str| {
        facts
            .symbols
            .iter()
            .find(|s| s.qualified_name == name)
            .unwrap()
    };
    let device = cpp
        .records
        .iter()
        .find(|r| r.symbol_id == symbol("demo::Device").id)
        .unwrap();
    assert!(device.is_definition && device.lookup_supported);
    let bases = device.bases.as_ref().unwrap();
    assert_eq!(bases.len(), 1);
    assert_eq!(bases[0].declared_type.name, "Base");
    assert!(bases[0].virtual_);
    assert_eq!(
        &source[bases[0].range.start_byte as usize..bases[0].range.end_byte as usize],
        "Base"
    );
    let derived = cpp
        .records
        .iter()
        .find(|r| r.symbol_id == symbol("demo::Derived").id)
        .unwrap();
    let bases = derived.bases.as_ref().unwrap();
    assert_eq!(bases.len(), 2);
    assert_eq!(bases[0].declared_type.name, "Handle");
    assert_eq!(bases[0].declared_type.template_arguments[0].name, "Device");
    assert!(!bases[0].virtual_ && bases[1].virtual_);
    assert_eq!(
        &source[bases[0].range.start_byte as usize..bases[0].range.end_byte as usize],
        "Handle<Device>"
    );
    for (name, pointer, reference, constant) in [
        ("demo::make", false, false, false),
        ("demo::borrow", false, true, true),
        ("demo::constant", true, false, true),
    ] {
        let ty = cpp
            .callables
            .iter()
            .find(|c| c.symbol_id == symbol(name).id)
            .unwrap()
            .return_type
            .as_ref()
            .unwrap();
        assert_eq!(ty.name, "Handle");
        assert_eq!(ty.template_arguments.len(), 1);
        assert_eq!(ty.template_arguments[0].name, "Device");
        assert_eq!(
            (ty.pointer, ty.reference, ty.const_),
            (pointer, reference, constant)
        );
    }
    let value = cpp
        .values
        .iter()
        .find(|v| v.initializer_call.is_some())
        .unwrap();
    assert!(value.declared_type.is_none());
    let call = facts
        .references
        .iter()
        .find(|r| Some(r.id) == value.initializer_call)
        .unwrap();
    assert_eq!(call.name, "make");
    assert_eq!(
        &source[value.declaration_range.start_byte as usize
            ..value.declaration_range.end_byte as usize],
        "auto device = make();"
    );
}

#[test]
fn reference_returning_template_body_owns_its_calls_instead_of_adjacent_prototype() {
    let source = r#"
struct Registry {
    void* previous();
    template<class T> static T& get() { T::prepare(); return T::instance(); }
    int next() { return after(); }
};
"#;
    let facts = extract(source, ExtractionMode::Structural);
    for reference in facts
        .references
        .iter()
        .filter(|r| r.kind == types::ReferenceKind::Call)
    {
        let owner = facts
            .symbols
            .iter()
            .find(|s| Some(s.id) == reference.source_symbol)
            .unwrap();
        let expected = if reference.name == "after" {
            "Registry::next"
        } else {
            "Registry::get"
        };
        assert_eq!(owner.qualified_name, expected, "{}", reference.text);
        assert!(
            owner.range.start_byte <= reference.range.start_byte
                && owner.range.end_byte >= reference.range.end_byte
        );
    }
    assert_eq!(
        facts
            .references
            .iter()
            .filter(|r| r.kind == types::ReferenceKind::Call)
            .count(),
        3
    );
}

#[test]
fn local_elaborated_types_and_prototypes_cannot_take_over_the_enclosing_callers() {
    let source = r#"
void sink() {}
void entry() {
    ::sink();
    struct External payload;
    void declared_only();
    ::sink();
    struct Local { void run() { ::sink(); } };
}

"#;
    let facts = extract(source, ExtractionMode::Structural);
    let calls: Vec<_> = facts
        .references
        .iter()
        .filter(|r| r.kind == types::ReferenceKind::Call)
        .collect();
    assert_eq!(calls.len(), 3);
    for symbol in facts
        .symbols
        .iter()
        .filter(|s| matches!(s.name.as_str(), "External" | "declared_only"))
    {
        assert_eq!(
            symbol.range, symbol.name_range,
            "a declaration cannot borrow entry's body: {}",
            symbol.name
        );
    }
    for (index, call) in calls.iter().enumerate() {
        let owner = facts
            .symbols
            .iter()
            .find(|s| Some(s.id) == call.source_symbol)
            .unwrap();
        assert!(matches!(
            owner.kind,
            SymbolKind::Function | SymbolKind::Method
        ));
        assert_eq!(owner.name, if index < 2 { "entry" } else { "run" });
        assert!(
            owner.range.start_byte <= call.range.start_byte
                && owner.range.end_byte >= call.range.end_byte
        );
    }
}

#[test]
fn qualified_class_definitions_share_names_ranges_and_member_owners() {
    for qualifier in ["Outer::Inner", "Outer /* owner */ :: Inner"] {
        let source = format!(
            "namespace api {{ struct Outer {{ struct Inner; }}; struct {qualifier} {{ int value; int read() {{ return value; }} struct Forward; }}; }}"
        );
        let facts = extract(&source, ExtractionMode::Structural);
        let cpp = facts.cpp_types.as_ref().unwrap();
        let class = facts
            .symbols
            .iter()
            .find(|s| s.qualified_name == "api::Outer::Inner" && s.range != s.name_range)
            .expect("the qualified definition must retain its own class body");
        assert_eq!(class.name, "Inner");
        assert_eq!(
            &source[class.name_range.start_byte as usize..class.name_range.end_byte as usize],
            "Inner"
        );
        let declarations: Vec<_> = cpp
            .records
            .iter()
            .filter(|r| r.symbol_id == class.id)
            .collect();
        assert!(declarations.iter().any(|r| !r.is_definition));
        let definitions: Vec<_> = declarations.iter().filter(|r| r.is_definition).collect();
        assert_eq!(definitions.len(), 1);
        assert!(definitions[0].lookup_supported);
        for member in ["value", "read", "Forward"] {
            let symbol = facts
                .symbols
                .iter()
                .find(|s| s.qualified_name == format!("api::Outer::Inner::{member}"))
                .unwrap();
            if member != "Forward" {
                assert_eq!(symbol.container, Some(class.id), "{symbol:?}");
            }
            assert!(
                class.range.start_byte <= symbol.range.start_byte
                    && symbol.range.end_byte <= class.range.end_byte
            );
            if member == "Forward" {
                assert_eq!(symbol.range, symbol.name_range);
            }
        }
        let field = facts
            .symbols
            .iter()
            .find(|s| s.qualified_name == "api::Outer::Inner::value")
            .unwrap();
        assert_eq!(
            cpp.values
                .iter()
                .find(|v| v.symbol_id == Some(field.id))
                .unwrap()
                .lookup_scope,
            "api::Outer::Inner"
        );
    }
    let source = "namespace api { struct Outer { struct Inner; }; } struct api::Outer::Inner { int field; };";
    for mode in [ExtractionMode::Structural, ExtractionMode::Manifest] {
        let facts = extract(source, mode.clone());
        let class = facts
            .symbols
            .iter()
            .find(|s| s.name_range.start_byte as usize == source.rfind("Inner").unwrap())
            .unwrap();
        assert_eq!(class.name, "Inner");
        assert_eq!(class.qualified_name, "api::Outer::Inner");
        if matches!(mode, ExtractionMode::Structural) {
            assert_ne!(class.range, class.name_range);
        }
    }
    let templated = extract(
        "template<class T> struct Outer { struct Inner; }; template<class T> struct Outer<T>::Inner { int read() { return 1; } };",
        ExtractionMode::Structural,
    );
    assert!(!templated.symbols.iter().any(|s| s.qualified_name == "read"));
    assert!(
        templated
            .symbols
            .iter()
            .any(|s| s.qualified_name == "Outer<T>::Inner::read")
    );
}

#[test]
fn class_forward_declarations_do_not_borrow_the_enclosing_class_body() {
    let source = r#"
int value() { return 1; }
struct Owner;
struct Owner {
    struct Forward;
    struct External* pointer;
    int initialized = ::value();
    void declared_only();
};
template<class T> struct Holder { int initialized = ::value(); };
"#;
    let facts = extract(source, ExtractionMode::Structural);
    for symbol in facts
        .symbols
        .iter()
        .filter(|s| matches!(s.name.as_str(), "Forward" | "External" | "declared_only"))
    {
        assert_eq!(symbol.range, symbol.name_range, "{}", symbol.name);
    }
    let calls = facts
        .references
        .iter()
        .filter(|r| r.kind == types::ReferenceKind::Call);
    let cpp = facts.cpp_types.as_ref().unwrap();
    let mut owners = Vec::new();
    for call in calls {
        let owner = facts
            .symbols
            .iter()
            .find(|s| Some(s.id) == call.source_symbol)
            .unwrap();
        assert!(
            !cpp.unverified_callable_scopes.contains(&owner.id),
            "{}",
            owner.qualified_name
        );
        owners.push(owner.qualified_name.as_str());
    }
    assert_eq!(owners, ["Owner", "Holder"]);
}

#[test]
fn operator_declarations_keep_identity_body_owner_and_written_return_type() {
    let source = r#"
namespace api {
struct Item {};
Item* acquire();
struct Handle { Item* operator ->() const; };
Item* Handle::operator->() const { return acquire(); }
template<class T> struct Box {
    T* operator /* spelling */ ->() const { return pointer; }
    T* pointer;
};
}
"#;
    let facts = extract(source, ExtractionMode::Structural);
    let ordinary: Vec<_> = facts
        .symbols
        .iter()
        .filter(|s| s.qualified_name == "api::Handle::operator->")
        .collect();
    assert_eq!(ordinary.len(), 2);
    assert_eq!(
        ordinary.iter().filter(|s| s.range != s.name_range).count(),
        1
    );
    let templated = facts
        .symbols
        .iter()
        .find(|s| s.qualified_name == "api::Box::operator->")
        .unwrap();
    assert!(
        source[templated.range.start_byte as usize..templated.range.end_byte as usize]
            .contains("return pointer")
    );
    let declaration = facts
        .cpp_types
        .as_ref()
        .unwrap()
        .callables
        .iter()
        .find(|c| c.symbol_id == templated.id)
        .unwrap();
    let returned = declaration.return_type.as_ref().unwrap();
    assert_eq!(returned.name, "T");
    assert!(returned.pointer);
    assert_eq!(declaration.qualifiers, "const");
    let call = facts
        .references
        .iter()
        .find(|r| r.kind == types::ReferenceKind::Call && r.name == "acquire")
        .unwrap();
    let owner = ordinary
        .iter()
        .find(|s| Some(s.id) == call.source_symbol)
        .unwrap();
    assert!(owner.range != owner.name_range);
}

#[test]
fn annotation_wrappers_use_actual_macro_replacements_when_shadowed() {
    use std::collections::HashMap;
    use types::cpp::CppAnnotationPosition;

    for (replacement, expected) in [
        ("", true),
        ("#define __attribute__(args)\n", true),
        ("#define __attribute__(args) __declspec(dllexport)\n", true),
        ("#define __attribute__(args) virtual\n", false),
        ("#define __attribute__(args)\n#undef __attribute__\n", false),
        (
            "#if PLATFORM\n#define __attribute__(args)\n#else\n#define __attribute__(args) virtual\n#endif\n",
            false,
        ),
        ("#define visibility unexpected\n", false),
        ("#define __attribute__(args) UNKNOWN(args)\n", false),
        ("#define __attribute__(args) __attribute__(args)\n", false),
    ] {
        let source = format!(
            "{replacement}#define API __attribute__((visibility(\"hidden\")))\nstruct Device {{ API int read(); }};\n"
        );
        let facts = extract(&source, ExtractionMode::Structural);
        let cpp = facts.cpp_types.as_ref().unwrap();
        let mut definitions = HashMap::<_, Vec<_>>::new();
        for definition in &cpp.macros {
            definitions
                .entry(definition.name.as_str())
                .or_default()
                .push(definition);
        }
        assert_eq!(
            extraction::cpp_annotations::is_annotation(
                "API",
                &definitions,
                &CppAnnotationPosition::DeclarationPrefix,
            ),
            expected,
            "{source}",
        );
    }
}

#[test]
fn declaration_prefix_annotations_keep_attributes_types_and_semantic_macros() {
    use std::collections::HashMap;
    use types::cpp::CppAnnotationPosition;
    for attributes in [
        "[[clang::lto_visibility_public]]",
        "\n[[nodiscard]] /* retain this */ [[deprecated(\"API\")]]\n",
    ] {
        let source = format!(
            r#"
#define API __attribute__((visibility("default")))
#define IMPORT __declspec(dllimport)
#define SEMANTIC virtual
#define TYPE int
namespace sample {{
class {attributes} API Device {{ public: [[nodiscard]] /* preserve */ API int read(); }};
struct Other {{ SEMANTIC int read(); TYPE value; Missing member; }};
}}
"#
        );
        let facts = extract(&source, ExtractionMode::Structural);
        let cpp = facts.cpp_types.as_ref().unwrap();
        let mut definitions = HashMap::<_, Vec<_>>::new();
        for definition in &cpp.macros {
            definitions
                .entry(definition.name.as_str())
                .or_default()
                .push(definition);
        }
        let annotations: Vec<_> = cpp
            .annotation_candidates
            .iter()
            .filter(|site| {
                extraction::cpp_annotations::is_annotation(&site.text, &definitions, &site.position)
            })
            .cloned()
            .collect();
        assert_eq!(annotations.len(), 2, "{source}: {annotations:?}");
        assert_eq!(annotations[0].position, CppAnnotationPosition::ClassPrefix);
        assert_eq!(
            annotations[1].position,
            CppAnnotationPosition::DeclarationPrefix
        );
        for site in &annotations {
            assert_eq!(site.text, "API");
            assert_eq!(
                &source[site.range.start_byte as usize..site.range.end_byte as usize],
                site.text
            );
            let prefix = &source[..site.range.start_byte as usize];
            assert_eq!(
                site.range.start_line as usize,
                prefix.bytes().filter(|b| *b == b'\n').count()
            );
            assert_eq!(
                site.range.start_column as usize,
                prefix.rsplit('\n').next().unwrap().len()
            );
        }
        assert!(extraction::cpp_annotations::is_annotation(
            "IMPORT",
            &definitions,
            &CppAnnotationPosition::DeclarationPrefix
        ));
        for name in ["SEMANTIC", "TYPE", "Missing"] {
            assert!(!extraction::cpp_annotations::is_annotation(
                name,
                &definitions,
                &CppAnnotationPosition::DeclarationPrefix
            ));
        }
        let frontend = extraction::cpp_annotations::frontend(annotations, Vec::new()).unwrap();
        let parser_source = frontend.parser.parser_source(&source);
        assert!(parser_source.contains(attributes));
        let normalized = extract_file_with_mode(
            &frontend,
            FileId::generate("qualified.cpp"),
            Path::new("qualified.cpp"),
            &source,
            "fixture",
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();
        assert!(
            normalized
                .symbols
                .iter()
                .any(|s| s.qualified_name == "sample::Device::read")
        );
    }
    // A lone class name or a call is not a declaration-prefix annotation site.
    let source = "#define API\nclass [[maybe_unused]] API {}; void entry() { API(); }";
    let facts = extract(source, ExtractionMode::Structural);
    assert!(
        facts
            .cpp_types
            .unwrap()
            .annotation_candidates
            .iter()
            .all(|site| site.text != "API")
    );
}

#[test]
fn literal_conditional_macro_branches_do_not_guess_build_configuration() {
    use std::collections::HashMap;
    use types::cpp::CppAnnotationPosition;
    for (source, expected) in [
        ("#if 1\n#define API\n#else\n#undef API\n#endif\n", true),
        (
            "#if 0\n#define API BAD\n#elif 0\n#undef API\n#else\n#define API\n#endif\n",
            true,
        ),
        (
            "#if 1\n#define API\n#elif CONFIG\n#define API BAD\n#else\n#undef API\n#endif\n",
            true,
        ),
        (
            "#if CONFIG\n#define API\n#else\n#undef API\n#endif\n",
            false,
        ),
        ("#if 1+0\n#define API\n#else\n#undef API\n#endif\n", false),
        (
            "#if 0\n#if CONFIG\n#define API BAD\n#endif\n#endif\n#define API\n",
            true,
        ),
    ] {
        let facts = extract(source, ExtractionMode::Structural);
        let cpp = facts.cpp_types.unwrap();
        let mut macros = HashMap::<_, Vec<_>>::new();
        for definition in &cpp.macros {
            macros
                .entry(definition.name.as_str())
                .or_default()
                .push(definition);
        }
        assert_eq!(
            extraction::cpp_annotations::is_annotation(
                "API",
                &macros,
                &CppAnnotationPosition::ClassPrefix
            ),
            expected,
            "{source}"
        );
        // Many independent empty annotations must respect the recursive work
        // bound as well as the byte/token bound, without overflowing the stack.
        assert!(!extraction::cpp_annotations::is_annotation(
            &"API ".repeat(200),
            &macros,
            &CppAnnotationPosition::ClassPrefix
        ));
    }
}

#[test]
fn annotation_selectors_preserve_runtime_forms_and_reject_argument_prescanning() {
    use std::collections::HashMap;
    use types::cpp::CppAnnotationPosition;
    let definitions = r#"
#define ATTRIBUTE(x) __attribute__((x))
#if 0
#define SINGLE(lock) UNAVAILABLE_ATTRIBUTE(lock)
#else
#define SINGLE(lock) ATTRIBUTE(acquire_capability(lock)) ATTRIBUTE(release_capability(lock))
#endif
#define PAIR(lock, expr) (guard(lock), expr)
#define CHOOSE(a, b, chosen, ...) chosen
#define ANNOTATE(...) CHOOSE(__VA_ARGS__, PAIR, SINGLE, )(__VA_ARGS__)
"#;
    for (extra, input, expected) in [
        ("", "ANNOTATE(gate)", true),
        ("", "ANNOTATE(gate, work())", false),
        ("", "ANNOTATE(gate) ANNOTATE(other)", true),
        ("#define gate extra, tokens\n", "ANNOTATE(gate)", false),
        ("#define ARG() gate\n", "ANNOTATE(ARG())", false),
        (
            "#define SINGLE(lock) UNKNOWN(lock)\n",
            "ANNOTATE(gate)",
            false,
        ),
        (
            "#define SINGLE(lock) ANNOTATE(lock)\n",
            "ANNOTATE(gate)",
            false,
        ),
        ("#undef SINGLE\n", "ANNOTATE(gate)", false),
        (
            "#define CHOOSE(a,b,c,...) c ## a\n",
            "ANNOTATE(gate)",
            false,
        ),
    ] {
        let facts = extract(
            &format!("{definitions}\n{extra}"),
            ExtractionMode::Structural,
        );
        let cpp = facts.cpp_types.unwrap();
        let mut macros = HashMap::<_, Vec<_>>::new();
        for definition in &cpp.macros {
            macros
                .entry(definition.name.as_str())
                .or_default()
                .push(definition);
        }
        for position in [
            CppAnnotationPosition::CallableSuffix,
            CppAnnotationPosition::LambdaSuffix,
        ] {
            assert_eq!(
                extraction::cpp_annotations::is_annotation(input, &macros, &position),
                expected,
                "{input} / {extra} / {position:?}"
            );
        }
        assert!(!extraction::cpp_annotations::is_annotation(
            input,
            &macros,
            &CppAnnotationPosition::DataSuffix
        ));
    }
    // An unknown condition retains all alternatives; a literal-false branch
    // is the only exclusion used by this fixture.
    let facts = extract(
        &definitions.replace("#if 0", "#if CONFIG"),
        ExtractionMode::Structural,
    );
    let cpp = facts.cpp_types.unwrap();
    let mut macros = HashMap::<_, Vec<_>>::new();
    for definition in &cpp.macros {
        macros
            .entry(definition.name.as_str())
            .or_default()
            .push(definition);
    }
    assert!(!extraction::cpp_annotations::is_annotation(
        "ANNOTATE(gate)",
        &macros,
        &CppAnnotationPosition::LambdaSuffix
    ));
}

#[test]
fn class_annotation_macros_keep_raw_locations_and_require_supported_replacements() {
    use std::collections::HashMap;
    let source = r#"
#if PLATFORM_A
#define VISIBLE __attribute__((visibility("default")))
#else
#define VISIBLE __declspec(dllexport)
#endif
#define API VISIBLE
namespace api {
class API Device final { public: int read() const { return 7; } };
}
"#;
    let raw = extract(source, ExtractionMode::Structural);
    let cpp = raw.cpp_types.as_ref().unwrap();
    let mut definitions = HashMap::<_, Vec<_>>::new();
    for definition in &cpp.macros {
        definitions
            .entry(definition.name.as_str())
            .or_default()
            .push(definition);
    }
    let site = cpp
        .annotation_candidates
        .iter()
        .find(|site| site.name == "API")
        .unwrap();
    assert!(extraction::cpp_annotations::is_annotation(
        "API",
        &definitions,
        &site.position,
    ));
    assert_eq!(
        &source[site.range.start_byte as usize..site.range.end_byte as usize],
        "API"
    );
    let frontend = extraction::cpp_annotations::frontend(vec![site.clone()], Vec::new()).unwrap();
    let parser_source = frontend.parser.parser_source(source);
    assert_eq!(parser_source.len(), source.len());
    assert_eq!(
        &parser_source[site.range.start_byte as usize..site.range.end_byte as usize],
        "   "
    );
    let mut changed = source.to_string();
    changed.replace_range(
        site.range.start_byte as usize..site.range.end_byte as usize,
        "NEW",
    );
    assert_eq!(frontend.parser.parser_source(&changed), changed);
    let normalized = extract_file_with_mode(
        &frontend,
        raw.file.file_id,
        Path::new("qualified.cpp"),
        source,
        "fixture",
        ExtractionMode::Structural,
        &(),
    )
    .unwrap();
    assert_eq!(normalized.file.status, types::ParseStatus::Success);
    let method = normalized
        .symbols
        .iter()
        .find(|s| s.qualified_name == "api::Device::read")
        .unwrap();
    assert_eq!(method.kind, SymbolKind::Method);
    assert_eq!(
        &source[method.range.start_byte as usize..method.range.end_byte as usize],
        "int read() const { return 7; }"
    );
    assert!(
        normalized
            .cpp_types
            .unwrap()
            .records
            .iter()
            .any(|r| r.is_definition && r.lookup_supported)
    );

    for replacements in [
        "#define API MISSING\n",
        "#define API OTHER\n#define OTHER API\n",
        "#define API(x) __attribute__((visibility(\"default\")))\n",
        "#define API __attribute__((vector_size(16)))\n",
        "#define API __attribute__((visibility(\"de fault\")))\n",
        "#define API __attribute__((visibility(\"default\")))\n#undef API\n",
        "#if PLATFORM_A\n#define API __declspec(dllexport)\n#else\n#define API WrongName\n#endif\n",
    ] {
        let text =
            format!("{replacements}class API Device {{ public: int read() {{ return 0; }} }};");
        let raw = extract(&text, ExtractionMode::Structural);
        let cpp = raw.cpp_types.unwrap();
        let mut definitions = HashMap::<_, Vec<_>>::new();
        for definition in &cpp.macros {
            definitions
                .entry(definition.name.as_str())
                .or_default()
                .push(definition);
        }
        assert!(
            !extraction::cpp_annotations::is_annotation(
                "API",
                &definitions,
                &types::cpp::CppAnnotationPosition::ClassPrefix,
            ),
            "{text}: {cpp:#?}"
        );
    }
}

#[test]
fn stringized_macro_arguments_preserve_member_names_with_explicit_limits() {
    use std::collections::HashMap;
    use types::{TextRange, cpp::CppMemberMacro};
    let cases = [
        ("const char* label() { return #X; }", "Widget", true),
        ("const char* label() { return #X; }", "", true),
        ("const char* label() { return #X; }", "123", true),
        ("const char* label() { return #X; }", "'x'", true),
        ("const char* label() { return #X; }", r#""a\"b""#, true),
        ("const char* label() { return #X; }", "+", true),
        ("const char* label() { return #X; }", "a+b", false),
        ("const char* label() { return #X; }", "outer::Widget", false),
        ("const char* label() { return #missing; }", "Widget", false),
        ("const char* label() { return #; }", "Widget", false),
        (
            "const char* label() { return #X ## suffix; }",
            "Widget",
            false,
        ),
        (
            "const char* label() { return prefix ## #X; }",
            "Widget",
            false,
        ),
    ];
    for (replacement, argument, supported) in cases {
        // Test the inventory of an already recorded invocation. Raw site
        // extraction is independently covered by the indexing test below.
        let source = format!("#define MEMBERS(X) {replacement}\n");
        let facts = extract(&source, ExtractionMode::Structural)
            .cpp_types
            .unwrap();
        let site = CppMemberMacro {
            scope: "Record".into(),
            name: "MEMBERS".into(),
            text: format!("MEMBERS({argument});"),
            range: TextRange::default(),
        };
        let mut definitions = HashMap::<_, Vec<_>>::new();
        for definition in &facts.macros {
            definitions
                .entry(definition.name.as_str())
                .or_default()
                .push(definition);
        }
        let names = extraction::cpp_member_macros::introduced_names(&site, &definitions);
        assert_eq!(
            names,
            supported.then(|| vec!["label".to_owned()]),
            "{source}"
        );
    }
}
