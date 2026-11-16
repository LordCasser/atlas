use std::{collections::BTreeMap, path::Path, sync::Arc};

use db::Store;
use extraction::{ExtractionMode, create_frontend, extract_file_with_mode};
use resolution::ReferenceResolver;
use types::{FileId, Language, ReferenceKind};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    name: String,
    path: String,
    has_body: bool,
}

fn analyze(files: &[(&str, &str)], scoped: bool) -> BTreeMap<String, Option<String>> {
    analyze_targets(files, scoped)
        .into_iter()
        .map(|(call, target)| (call, target.map(|t| t.name)))
        .collect()
}

fn analyze_targets(files: &[(&str, &str)], scoped: bool) -> BTreeMap<String, Option<Target>> {
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let mut file_ids = Vec::new();
    for (path, source) in files {
        let id = FileId::generate(path);
        file_ids.push(id);
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            id,
            Path::new(path),
            source,
            "fixture",
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();
        store.insert_file_facts(&facts).unwrap();
    }
    let mut resolver = ReferenceResolver::new(store.clone());
    let (resolved, _) = if scoped {
        store.insert_closure_generation("cpp-test").unwrap();
        resolver
            .resolve_for_closure("cpp-test", 0, &file_ids, None)
            .unwrap()
    } else {
        resolver
            .resolve_all_parallel(store.clone(), None, None)
            .unwrap()
    };
    let symbols: BTreeMap<_, _> = store
        .get_all_symbols()
        .unwrap()
        .into_iter()
        .map(|s| (s.id, s))
        .collect();
    let targets: BTreeMap<_, _> = resolved
        .into_iter()
        .filter(|(_, target)| target.strategy != types::ResolutionStrategy::ImplicitOperator)
        .map(|(r, t)| {
            let symbol = &symbols[&t.symbol_id];
            let path = files
                .iter()
                .find(|(path, _)| FileId::generate(path) == symbol.file_id)
                .unwrap()
                .0;
            (
                r.id,
                Target {
                    name: symbol.qualified_name.clone(),
                    path: path.into(),
                    has_body: symbol.range != symbol.name_range,
                },
            )
        })
        .collect();
    let mut calls = BTreeMap::new();
    for file in file_ids {
        for reference in store.find_references_by_file(&file).unwrap() {
            if reference.kind == ReferenceKind::Call {
                let caller = reference
                    .source_symbol
                    .map(|id| symbols[&id].qualified_name.as_str())
                    .unwrap_or("?");
                let key = format!("{caller}:{}", reference.text);
                calls.insert(key, targets.get(&reference.id).cloned());
            }
        }
    }
    calls
}

// These fixtures identify a particular closure by its source position. Keep
// target assertions unchanged while requiring the call to belong to that
// closure, rather than merging it back into its enclosing named function.
fn lambda_call_key(source: &str, after: &str, call: &str, owner: &str, expression: &str) -> String {
    let start = source.find(after).unwrap();
    let position = start + source[start..].find(call).unwrap();
    let capture = source[..position].rfind('[').unwrap();
    format!("{owner}<lambda@{capture}>:{expression}")
}

#[test]
fn integral_argument_applicability_does_not_require_an_exact_promoted_type() {
    let header = r#"
namespace api {
using Byte = unsigned char;
int accept(Byte value);
struct Input { Byte value; };
}
namespace unrelated { int accept(unsigned char value); }
"#;
    let body = r#"
#include "api.h"
int api::accept(Byte value) { return value; }
"#;
    let uses = r#"
#include "api.h"
namespace api {
int exact(Byte value) { return accept(value); }
int converted(int value) { return accept(value); }
int mixed(Byte value) { return accept(value + 1); }
int reversed(Byte value) { return accept(1 + value); }
int nested(unsigned short first, long second) { return accept((first + second) * 2); }
int field(const Input& input) { return accept(input.value + 1); }
int macro_name(Byte operand) {
#define operand(...) unused_replacement
return accept(operand + 1);
}
#undef operand
}
"#;
    let files = [("api.h", header), ("api.cpp", body), ("uses.cpp", uses)];
    let actual = analyze_targets(&files, false);
    assert_eq!(actual, analyze_targets(&files, true));
    for caller in [
        "exact",
        "converted",
        "mixed",
        "reversed",
        "nested",
        "field",
        "macro_name",
    ] {
        assert_eq!(
            actual[&format!("api::{caller}:accept")],
            Some(Target {
                name: "api::accept".into(),
                path: "api.cpp".into(),
                has_body: true,
            }),
            "{caller}: {actual:?}"
        );
    }
}

#[test]
fn integral_argument_properties_do_not_rank_competing_or_unestablished_calls() {
    let source = r#"
namespace overloads {
int accept(int value); int accept(unsigned int value);
int run(unsigned char value) { return accept(value + 1); }
}
namespace templates {
int accept(unsigned char value); template<class T> int accept(T value);
int run(unsigned char value) { return accept(value + 1); }
}
namespace reference {
int accept(int& value);
int run(int value) { return accept(value + 1); }
}
namespace converted_reference {
int accept(const long& value);
int run(unsigned char value) { return accept(value + 1); }
}
namespace pointer {
int accept(int* value);
int run(unsigned char value) { return accept(value + 1); }
}
namespace object {
struct Value {}; Value operator+(Value, int); int accept(unsigned char value);
int run(Value value) { return accept(value + 1); }
}
namespace missing_type {
int accept(unsigned char value);
int run(Missing value) { return accept(value + 1); }
}
namespace replaced_operand {
struct Value {}; Value operator+(Value, int); int accept(unsigned char value);
int run(unsigned char value) {
#define value Value()
return accept(value + 1);
}
#undef value
int converted(int count) {
#define count Value()
return accept(count);
}
#undef count
}
namespace replaced_other_argument {
struct Value {}; int accept(int first, unsigned char second);
int run(int left_value, int right_value) {
#define left_value Value()
return accept(left_value, right_value);
}
#undef left_value
}
namespace replaced_class_argument {
struct Value {}; int accept(Value first, unsigned char second);
int run(Value object_value, int integer_value) {
#define object_value 0
return accept(object_value, integer_value);
}
#undef object_value
}
namespace existing {
int accept(const int& value) { return value; }
int run(int value) { return accept(value + 1); }
}
"#;
    let actual = analyze(&[("calls.cpp", source)], false);
    assert_eq!(actual, analyze(&[("calls.cpp", source)], true));
    for scope in [
        "overloads",
        "templates",
        "reference",
        "converted_reference",
        "pointer",
        "object",
        "missing_type",
        "replaced_operand",
        "replaced_other_argument",
        "replaced_class_argument",
    ] {
        assert_eq!(actual[&format!("{scope}::run:accept")], None, "{actual:?}");
    }
    assert_eq!(
        actual["existing::run:accept"],
        Some("existing::accept".into())
    );
    assert_eq!(
        actual["replaced_operand::converted:accept"], None,
        "{actual:?}"
    );
}

#[test]
fn field_argument_types_use_member_identity_and_qualified_declaration_scope() {
    let declarations = r#"
namespace data { struct Value {}; int select(Value value) { return 1; } }
namespace model {
struct Item { data::Value value; };
struct Holder { Item item; };
struct Derived : Item {};
struct Hidden : Item { int value; };
struct Left { data::Value value; };
struct Right { data::Value value; };
struct Ambiguous : Left, Right {};
struct Wrapper { Item* operator->(); };
int select(const Holder& holder) { return select(holder.item.value); }
}
"#;
    let uses = r#"
#include "types.h"
struct Value {};
int select(int value) { return 2; }
int local(const model::Item& item) { return select(item.value); }
int pointer(const model::Item* item) { return select(item->value); }
int parentheses(const model::Item& item) { return select((item).value); }
int inherited(const model::Derived& item) { return select(item.value); }
int hidden(const model::Hidden& item) { return select(item.value); }
int ambiguous(const model::Ambiguous& item) { return select(item.value); }
int missing(const model::Item& item) { return select(item.absent); }
int indirect(model::Wrapper item) { return select(item->value); }
"#;
    let files = [("types.h", declarations), ("uses.cpp", uses)];
    let actual = analyze_targets(&files, false);
    assert_eq!(actual, analyze_targets(&files, true));
    for caller in [
        "model::select",
        "local",
        "pointer",
        "parentheses",
        "inherited",
    ] {
        assert_eq!(
            actual[&format!("{caller}:select")],
            Some(Target {
                name: "data::select".into(),
                path: "types.h".into(),
                has_body: true,
            }),
            "{caller}: {actual:?}"
        );
    }
    assert_eq!(
        actual["hidden:select"],
        Some(Target {
            name: "select".into(),
            path: "uses.cpp".into(),
            has_body: true,
        }),
        "the nearer int field must not borrow the base's Value type: {actual:?}"
    );
    for caller in ["ambiguous", "missing", "indirect"] {
        assert!(
            actual[&format!("{caller}:select")].is_none(),
            "{caller}: {actual:?}"
        );
    }
}

#[test]
fn field_arguments_preserve_object_cv_and_reference_member_rules() {
    let source = r#"
void writable(int& value) {}
void readable(const int& value) {}
void address(int* value) {}
struct Box { int value; mutable int cache; int& alias; static int shared; int* pointer; int bits : 3, ordinary; };
void direct(Box& box) { writable(box.value); }
void rejected(const Box& box) { writable(box.value); }
void readonly(const Box& box) { readable(box.value); }
void mutable_member(const Box& box) { writable(box.cache); }
void reference_member(const Box& box) { writable(box.alias); }
void static_member(const Box& box) { writable(box.shared); }
void pointer_member(const Box& box) { address(box.pointer); }
void volatile_member(volatile Box& box) { writable(box.value); }
void bitfield(Box& box) { writable(box.bits); }
void mixed_declaration(Box& box) { writable(box.ordinary); }
"#;
    let files = [("uses.cpp", source)];
    let actual = analyze(&files, false);
    assert_eq!(actual, analyze(&files, true));
    for (caller, callee) in [
        ("direct", "writable"),
        ("readonly", "readable"),
        ("mutable_member", "writable"),
        ("reference_member", "writable"),
        ("static_member", "writable"),
        ("pointer_member", "address"),
        ("mixed_declaration", "writable"),
    ] {
        assert_eq!(
            actual[&format!("{caller}:{callee}")].as_deref(),
            Some(callee),
            "{actual:?}"
        );
    }
    for caller in ["rejected", "volatile_member", "bitfield"] {
        assert!(
            actual[&format!("{caller}:writable")].is_none(),
            "{actual:?}"
        );
    }
}

#[test]
fn field_argument_names_do_not_ignore_object_macro_replacement() {
    let declarations = r#"
namespace data { struct Value {}; int select(Value value) { return 1; } }
struct Item { data::Value value; int other; };
int select(int value) { return 2; }
"#;
    for (directive, expected) in [
        ("#define value other", None),
        ("#define item other", None),
        ("#define value(...) other", Some("data::select")),
        ("#define unrelated other", Some("data::select")),
    ] {
        let uses = format!(
            "#include \"types.h\"\nint caller(Item& item, int other) {{\n{directive}\nreturn select(item.value); }}"
        );
        let actual = analyze(&[("types.h", declarations), ("uses.cpp", &uses)], false);
        assert_eq!(
            actual["caller:select"].as_deref(),
            expected,
            "{directive}: {actual:?}"
        );
    }
}

#[test]
fn cpp_library_short_names_do_not_replace_call_target_lookup() {
    for name in ["forward", "move", "find", "swap"] {
        let source = format!(
            r#"
int {name}(int value) {{ return value; }}
namespace project {{ int {name}(int value) {{ return value; }} }}
namespace external {{}}
struct Cursor {{ int {name}(int value) {{ return value; }} }};
struct Other {{}};
int plain(int value) {{ return {name}(value); }}
int qualified(int value) {{ return project::{name}(value); }}
int member(Cursor& cursor, int value) {{ return cursor.{name}(value); }}
int missing(int value) {{ return external::{name}(value); }}
int wrong_receiver(Other& cursor, int value) {{ return cursor.{name}(value); }}
int shadowed(int value) {{ int {name} = value; return {name}(value); }}
"#
        );
        let files = [("main.cpp", source.as_str())];
        let actual = analyze_targets(&files, false);
        assert_eq!(actual, analyze_targets(&files, true));
        for (caller, expression, target) in [
            ("plain", name.to_string(), name.to_string()),
            (
                "qualified",
                format!("project::{name}"),
                format!("project::{name}"),
            ),
            (
                "member",
                format!("cursor.{name}"),
                format!("Cursor::{name}"),
            ),
        ] {
            let key = format!("{caller}:{expression}");
            let target_fact = actual[&key]
                .as_ref()
                .unwrap_or_else(|| panic!("{key}: {actual:?}"));
            assert_eq!(target_fact.name, target);
            assert!(target_fact.has_body);
        }
        for (caller, expression) in [
            ("missing", format!("external::{name}")),
            ("wrong_receiver", format!("cursor.{name}")),
            ("shadowed", name.to_string()),
        ] {
            assert_eq!(actual[&format!("{caller}:{expression}")], None);
        }
    }
}

#[test]
fn selected_pointer_parameters_reject_known_arithmetic_arguments() {
    for scoped in [false, true] {
        for (label, source, call) in [
            (
                "qualified",
                "namespace target { void consume(int*) {} } void entry(int n) { target::consume(n); }",
                "entry:target::consume",
            ),
            (
                "member",
                "struct Target { void consume(int*) {} }; void entry(Target& t, int n) { t.consume(n); }",
                "entry:t.consume",
            ),
            (
                "static_member",
                "struct Target { static void consume(int*) {} }; void entry(int n) { Target::consume(n); }",
                "entry:Target::consume",
            ),
            (
                "implicit_member",
                "struct Target { void consume(int*) {} void entry(int n) { consume(n); } };",
                "Target::entry:consume",
            ),
            (
                "alias",
                "using Count = unsigned long; namespace target { void consume(void*) {} } void entry(Count n) { target::consume(n); }",
                "entry:target::consume",
            ),
            (
                "reference",
                "namespace target { void consume(int*) {} } void entry(int& n) { target::consume(n); }",
                "entry:target::consume",
            ),
            (
                "auto_return",
                "int number() { return 1; } namespace target { void consume(int*) {} } void entry() { auto n = number(); target::consume(n); }",
                "entry:target::consume",
            ),
            (
                "nonzero_literal",
                "namespace target { void consume(int*) {} } void entry() { target::consume(1); }",
                "entry:target::consume",
            ),
        ] {
            let source = format!("{source}\n");
            let calls = analyze(&[("main.cpp", &source)], scoped);
            assert_eq!(
                calls.get(call),
                Some(&None),
                "{label}, scoped={scoped}: {calls:?}"
            );
        }
    }
}

#[test]
fn argument_conflict_checks_preserve_pointer_conversions_and_unmodeled_facts() {
    for scoped in [false, true] {
        for (label, source) in [
            (
                "pointer",
                "namespace target { void consume(int*) {} } void entry(int* n) { target::consume(n); }",
            ),
            (
                "zero",
                "namespace target { void consume(int*) {} } void entry() { target::consume(0); }",
            ),
            (
                "nullptr",
                "namespace target { void consume(int*) {} } void entry() { target::consume(nullptr); }",
            ),
            (
                "class_conversion",
                "struct Value { operator int*() const; }; namespace target { void consume(int*) {} } void entry(Value n) { target::consume(n); }",
            ),
            (
                "template_conversion",
                "template<class T> struct Value { operator T*() const; }; namespace target { void consume(int*) {} } void entry(Value<int> n) { target::consume(n); }",
            ),
            (
                "legacy_const_zero",
                "namespace target { void consume(int*) {} } void entry() { const int n = 0; target::consume(n); }",
            ),
            (
                "macro_argument",
                "namespace target { void consume(int*) {} } void entry(int n) {\n#define n nullptr\n target::consume(n);\n#undef n\n }",
            ),
            (
                "implicit_numeric_conversion",
                "namespace target { void consume(double) {} } void entry(int n) { target::consume(n); }",
            ),
        ] {
            let source = format!("{source}\n");
            let calls = analyze(&[("main.cpp", &source)], scoped);
            assert_eq!(
                calls.get("entry:target::consume"),
                Some(&Some("target::consume".into())),
                "{label}, scoped={scoped}: {calls:?}"
            );
        }
    }
}

#[test]
fn explicit_template_type_arguments_preserve_identity_and_associated_lookup() {
    for scoped in [false, true] {
        for (label, source, expected) in [
            (
                "argument_namespace",
                "template<class T> struct Box {}; namespace item { struct Tag {}; void consume(Box<Tag>) {} } void entry(Box<item::Tag> value) { consume(value); }",
                Some("item::consume"),
            ),
            (
                "template_namespace",
                "namespace wrapper { template<class T> struct Box {}; void consume(Box<int>) {} } void entry(wrapper::Box<int> value) { consume(value); }",
                Some("wrapper::consume"),
            ),
            (
                "underlying_alias",
                "using Count = int; template<class T> struct Box {}; void consume(Box<int>) {} void entry(Box<Count> value) { consume(value); }",
                Some("consume"),
            ),
            (
                "distinct_instantiations",
                "namespace wrapper { template<class T> struct Box {}; void consume(Box<long>) {} } void consume(wrapper::Box<int>) {} void entry(wrapper::Box<int> value) { consume(value); }",
                Some("consume"),
            ),
            (
                "nested_type_argument",
                "template<class T> struct Box {}; namespace item { struct Tag {}; void consume(Box<Box<Tag>>) {} } void entry(Box<Box<item::Tag>> value) { consume(value); }",
                Some("item::consume"),
            ),
            (
                "unrecorded_pointer_type_argument",
                "namespace wrapper { template<class T> struct Box {}; void consume(Box<int>) {} } void consume(wrapper::Box<int*>) {} void entry(wrapper::Box<int*> value) { consume(value); }",
                None,
            ),
            (
                "const_type_argument",
                "namespace wrapper { template<class T> struct Box {}; void consume(Box<int>) {} } void consume(wrapper::Box<const int>) {} void entry(wrapper::Box<const int> value) { consume(value); }",
                Some("consume"),
            ),
            (
                "same_primary_different_arguments",
                "template<class T> struct Box {}; namespace first { struct Tag {}; } namespace second { struct Tag {}; void consume(Box<first::Tag>, Box<Tag>) {} } void entry(Box<first::Tag> a, Box<second::Tag> b) { consume(a, b); }",
                Some("second::consume"),
            ),
            (
                "argument_base_namespace",
                "template<class T> struct Box {}; namespace base { struct Base {}; } namespace item { struct Tag : base::Base {}; } namespace base { void consume(Box<item::Tag>) {} } void entry(Box<item::Tag> value) { consume(value); }",
                Some("base::consume"),
            ),
            (
                "wrong_argument_identity",
                "template<class T> struct Box {}; namespace first { struct Tag {}; } namespace second { struct Tag {}; } void consume(Box<first::Tag>) {} void entry(Box<second::Tag> value) { consume(value); }",
                None,
            ),
            (
                "hidden_friend_ambiguity",
                "template<class T> struct Box {}; namespace item { struct Tag { friend void consume(Box<Tag>); }; } void consume(Box<item::Tag>) {} void entry(Box<item::Tag> value) { consume(value); }",
                None,
            ),
            (
                "primary_template_friend_ambiguity",
                "namespace wrapper { template<class T> struct Box { friend void consume(Box) {} }; } void consume(wrapper::Box<int>) {} void entry(wrapper::Box<int> value) { consume(value); }",
                None,
            ),
            (
                "specialized_template",
                "template<class T> struct Box {}; template<> struct Box<int> {}; void consume(Box<int>) {} void entry(Box<int> value) { consume(value); }",
                None,
            ),
            (
                "dependent_base",
                "struct Tag {}; template<class T> struct Box : T {}; void consume(Box<Tag>) {} void entry(Box<Tag> value) { consume(value); }",
                None,
            ),
        ] {
            let source = format!("{source}\n");
            let calls = analyze(&[("main.cpp", &source)], scoped);
            assert_eq!(
                calls.get("entry:consume"),
                Some(&expected.map(str::to_owned)),
                "{label}, scoped={scoped}: {calls:?}"
            );
        }
    }
}

#[test]
fn ordinary_alias_identity_is_shared_by_arguments_receivers_and_inherited_types() {
    let header = r#"
namespace original {
struct Item { void run() {} };
void consume(Item) {}
using Scalar = int;
}
namespace names {
using Value = original::Item;
typedef original::Scalar Count;
using Quantity = Count;
void consume(int) {}
struct Base { using Number = int; };
}
"#;
    let source = r#"#include "types.hpp"
namespace names {
void entry(Value value) { consume(value); }
void object(Value* value) { value->run(); }
void integer(int) {}
void scalar(Quantity n) { integer(n); }
void arithmetic(Count n) { integer(n + n); }
struct Derived : Base { void entry(Number n) { integer(n); } };
void captured(Count n) { auto task = [n] { integer(n); }; }
}
namespace client {
struct Scalar {};
void consume(int) {}
void entry(original::Scalar n) { consume(n); }
}
"#;
    let files = [("types.hpp", header), ("main.cpp", source)];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    let captured = lambda_call_key(
        source,
        "void captured(",
        "integer(n);",
        "names::",
        "integer",
    );
    for (call, target) in [
        ("names::entry:consume", "original::consume"),
        ("names::object:value->run", "original::Item::run"),
        ("names::scalar:integer", "names::integer"),
        ("names::arithmetic:integer", "names::integer"),
        ("names::Derived::entry:integer", "names::integer"),
        (captured.as_str(), "names::integer"),
        ("client::entry:consume", "client::consume"),
    ] {
        let actual = result[call]
            .as_ref()
            .unwrap_or_else(|| panic!("{call}: {result:?}"));
        assert_eq!(actual.name, target);
        assert!(actual.has_body);
    }
}

#[test]
fn unknown_alias_targets_do_not_borrow_types_or_associated_namespaces() {
    let source = r#"
void take(int*) {}
namespace scalar { using Value = double; void entry(Value v) { take(v); } }
namespace pointers { using Value = int*; void consume(int) {} void entry(Value v) { consume(v); } }
namespace qualifiers { using Value = const int; void consume(int&) {} void entry(Value v) { consume(v); } }
namespace missing { using Value = Absent; void consume(Value) {} void entry(Value v) { consume(v); } }
namespace cycle { using Left = Right; using Right = Left; void consume(Left) {} void entry(Left v) { consume(v); } }
namespace local { void consume(int) {} void entry() { using Value=int; Value v=0; consume(v); } }
namespace template_alias { template<class T> using Value=T; void consume(int) {} void entry(Value<int> v) { consume(v); } }
namespace original { struct Item {}; }
namespace names { using Value=original::Item; void consume(Value) {} }
void entry(names::Value v) { consume(v); }
"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
    assert_eq!(result.len(), 8);
    assert!(result.values().all(Option::is_none), "{result:?}");
    // The caller includes a definition that was unavailable at the alias use.
    let files = [
        (
            "types.hpp",
            "namespace api { using Value=Missing; void consume(Value); }",
        ),
        ("late.hpp", "namespace api { struct Missing {}; }"),
        (
            "main.cpp",
            "#include \"types.hpp\"\n#include \"late.hpp\"\nvoid entry(api::Value v) { consume(v); }",
        ),
    ];
    let result = analyze(&files, false);
    assert_eq!(result, analyze(&files, true));
    assert_eq!(result["entry:consume"], None);
}

#[test]
fn lambda_argument_lookup_respects_capture_types_and_local_scope() {
    let source = r#"
#include "mutable.hpp"
#include "constant.hpp"
namespace direct { void consume(int) {} struct Owner { void entry(int value) { consume(value); } }; }
namespace copy { void consume(int) {} struct Owner { void entry(int value) { auto task = [value] { consume(value); }; } }; }
namespace absent { void consume(int) {} struct Owner { void entry(int value) { auto task = [] { consume(value); }; } }; }
namespace immutable { void consume(int&) {} struct Owner { void entry(int value) { auto task = [value] { consume(value); }; } }; }
namespace mut { void consume(int&) {} struct Owner { void entry(int value) { auto task = [value]() mutable { consume(value); }; } }; }
namespace ref { void consume(int&) {} struct Owner { void entry(int value) { auto task = [&value] { consume(value); }; } }; }
namespace default_copy { void consume(int&) {} struct Owner { void entry(int value) { auto task = [=] { consume(value); }; } }; }
namespace default_ref { void consume(int&) {} struct Owner { void entry(int value) { auto task = [&] { consume(value); }; } }; }
namespace init { struct Other {}; void consume(int) {} struct Owner { void entry(int value) { auto task = [value = Other{}] { consume(value); }; } }; }
namespace independent { void consume(int) {} struct Owner { void entry(int value) { auto task = [unused = 42, value] { consume(value); }; } }; }
namespace nested { void consume(int&) {} struct Owner { void entry(int value) { auto outer = [value] { auto inner = [value]() mutable { consume(value); }; }; } }; }
namespace nested_choice { void consume(int&) {} void consume(const int&) {} struct Owner { void entry(int value) { auto outer = [value] { auto inner = [value]() mutable { consume(value); }; }; } }; }
namespace broken_chain { void consume(int) {} struct Owner { void entry(int value) { auto outer = [] { auto inner = [value] { consume(value); }; }; } }; }
namespace nested_ref { void consume(int&) {} struct Owner { void entry(int value) { auto outer = [&] { auto inner = [&value] { consume(value); }; }; } }; }
namespace local { void consume(int&) {} struct Owner { void entry(double value) { auto task = [] { int value; consume(value); }; } }; }
namespace parameter { void consume(int&) {} struct Owner { void entry(double value) { auto task = [](int value) { consume(value); }; } }; }
namespace no_leak { void consume(double) {} void inner(int) {} struct Owner { void entry(double value) { auto task = [](int value) { inner(value); }; consume(value); } }; }
namespace static_local { void consume(int&) {} struct Owner { void entry() { static int value; auto task = [] { consume(value); }; } }; }
namespace pointer { void consume(int*) {} struct Owner { void entry(int* value) { auto task = [value] { consume(value); }; } }; }
namespace pointer_ref { void consume(int*&) {} struct Owner { void entry(int* value) { auto task = [value] { consume(value); }; } }; }
namespace cv { struct Owner { void entry(int value) { auto task = [value] { consume(value); }; } }; }
namespace boundary { int consume(int x) { return x; } struct Owner { void entry(int value) { auto task = [value = consume(value)] {}; } }; }
"#;
    let files = [
        (
            "mutable.hpp",
            "namespace cv { int consume(int& value) { return value; } }",
        ),
        (
            "constant.hpp",
            "namespace cv { int consume(const int& value) { return value; } }",
        ),
        ("main.cpp", source),
    ];
    for scoped in [false, true] {
        let calls = analyze_targets(&files, scoped);
        for name in [
            "direct",
            "copy",
            "mut",
            "ref",
            "default_ref",
            "independent",
            "nested_ref",
            "local",
            "parameter",
            "no_leak",
            "static_local",
            "pointer",
            "boundary",
        ] {
            let key = if matches!(name, "direct" | "no_leak" | "boundary") {
                format!("{name}::Owner::entry:consume")
            } else {
                lambda_call_key(
                    source,
                    &format!("namespace {name} {{"),
                    "consume(value);",
                    &format!("{name}::Owner::"),
                    "consume",
                )
            };
            assert_eq!(
                calls[&key].as_ref().map(|target| target.name.as_str()),
                Some(format!("{name}::consume").as_str()),
                "{key}: {calls:#?}"
            );
        }
        for name in [
            "absent",
            "immutable",
            "default_copy",
            "init",
            "nested",
            "nested_choice",
            "broken_chain",
            "pointer_ref",
        ] {
            assert_eq!(
                calls[&lambda_call_key(
                    source,
                    &format!("namespace {name} {{"),
                    "consume(value);",
                    &format!("{name}::Owner::"),
                    "consume"
                )],
                None,
                "{name}: {calls:#?}"
            );
        }
        assert_eq!(
            calls[&lambda_call_key(
                source,
                "namespace cv {",
                "consume(value);",
                "cv::Owner::",
                "consume"
            )]
                .as_ref()
                .map(|target| target.path.as_str()),
            Some("constant.hpp")
        );
    }
}

#[test]
fn character_arguments_select_their_declared_overload_without_guessing_encoding_or_dialect() {
    let headers = [
        ("char.hpp", "constexpr int select(char) { return 1; }"),
        ("int.hpp", "constexpr int select(int) { return 2; }"),
        ("wide.hpp", "constexpr int select(wchar_t) { return 3; }"),
        ("utf16.hpp", "constexpr int select(char16_t) { return 4; }"),
        ("utf32.hpp", "constexpr int select(char32_t) { return 5; }"),
    ];
    let cases = [
        ("'a'", Some("char.hpp")),
        (r"'\n'", Some("char.hpp")),
        (r"'\''", Some("char.hpp")),
        (r"'\\'", Some("char.hpp")),
        (r"'\0'", Some("char.hpp")),
        (r"'\101'", Some("char.hpp")),
        (r"'\x41'", Some("char.hpp")),
        (r"'\x80'", Some("char.hpp")),
        ("(/*same character*/'q')", Some("char.hpp")),
        ("L'x'", Some("wide.hpp")),
        (r"L'\n'", Some("wide.hpp")),
        (r"L'\x7fff'", None), // The underlying integer type is not recorded.
        ("u'Ω'", Some("utf16.hpp")),
        (r"u'\u03a9'", Some("utf16.hpp")),
        (r"u'\xffff'", Some("utf16.hpp")),
        ("U'😀'", Some("utf32.hpp")),
        (r"U'\U0001f600'", Some("utf32.hpp")),
        (r"U'\xffffffff'", Some("utf32.hpp")),
        ("'ab'", None),     // Conditionally-supported multicharacter literal.
        ("u8'a'", None),    // char in C++17, char8_t since C++20.
        ("L'Ω'", None),     // The wide execution encoding is not recorded.
        (r"'\x100'", None), // Needs more than the guaranteed char width.
        (r"'\q'", None),    // Conditional escape, not the character q.
        ("'a'_tag", None),  // A literal operator can return a class type.
        (r#""a""#, None),   // A string is not a character argument.
    ];
    let mut source = String::from(
        "#include \"char.hpp\"\n#include \"int.hpp\"\n#include \"wide.hpp\"\n#include \"utf16.hpp\"\n#include \"utf32.hpp\"\nstruct Token {}; Token operator\"\"_tag(char); int select(Token);\n",
    );
    for (i, (literal, _)) in cases.iter().enumerate() {
        source.push_str(&format!(
            "int entry_{i}() {{ return select({literal}); }}\n"
        ));
    }
    let mut files = headers.to_vec();
    files.push(("sample.cpp", &source));
    for scoped in [false, true] {
        let calls = analyze_targets(&files, scoped);
        for (i, (literal, expected)) in cases.iter().enumerate() {
            let target = calls
                .get(&format!("entry_{i}:select"))
                .unwrap_or_else(|| panic!("missing call: {literal}"));
            assert_eq!(
                target.as_ref().map(|t| t.path.as_str()),
                *expected,
                "{literal}: {calls:#?}"
            );
            if let Some(target) = target {
                assert!(target.has_body);
            }
        }
    }
}

#[test]
fn member_body_recovery_preserves_type_use_and_unrelated_members() {
    for body in [
        "return !this->operator==(other);",
        "return !((*this) == other);",
        "return unavailable(other);",
        "return @;", // Incomplete input: only this body is uninspectable.
    ] {
        let source = format!(
            r#"
namespace api {{
struct Value {{
    bool operator==(const Value&) const {{ return true; }}
    bool different(const Value& other) const {{ {body} }}
    int read() {{ return unfinished(); }}
}};
int take(Value) {{ return 2; }}
}}
int take(int) {{ return 1; }}
int entry(api::Value value) {{ return take(value); }}
int member(api::Value* value) {{ return value->read(); }}
"#
        );
        let files = [("sample.cpp", source.as_str())];
        let result = analyze_targets(&files, false);
        assert_eq!(result, analyze_targets(&files, true));
        for (call, expected) in [
            ("entry:take", "api::take"),
            ("member:value->read", "api::Value::read"),
        ] {
            let target = result[call]
                .as_ref()
                .unwrap_or_else(|| panic!("{body}: {result:#?}"));
            assert_eq!(target.name, expected);
            assert!(target.has_body);
        }
        assert_eq!(result.get("api::Value::read:unfinished"), Some(&None));
    }
}

#[test]
fn incomplete_member_declarations_still_limit_adl_and_member_selection() {
    for declaration in [
        "UNKNOWN_MEMBERS(Value)",
        "friend int take(Value);",
        "int @;",
    ] {
        let source = format!(
            r#"
namespace api {{ struct Value {{ {declaration} int read() {{ return 1; }} }}; }}
int take(api::Value) {{ return 2; }}
int entry(api::Value value) {{ return take(value); }}
int member(api::Value* value) {{ return value->read(); }}
"#
        );
        for scoped in [false, true] {
            let result = analyze(&[("sample.cpp", &source)], scoped);
            assert_eq!(
                result.get("entry:take"),
                Some(&None),
                "{declaration}: {result:#?}"
            );
            if !declaration.starts_with("friend") {
                assert_eq!(
                    result.get("member:value->read"),
                    Some(&None),
                    "{declaration}: {result:#?}"
                );
            }
        }
    }
}

#[test]
fn auto_arguments_reuse_visible_factory_returns_and_keep_unknown_initializers() {
    let header = r#"
namespace api {
struct Value { int read() { return 1; } };
Value make(int);
Value transform(Value, int);
const Value& borrow();
int take(Value);
}
int take(int);
"#;
    let source = r#"#include "api.hpp"
namespace other { struct Value {}; }
int chain(int n) { auto first = api::make(n); auto second = transform(first, n); return take(second); }
int copy() { auto value = api::borrow(); return take(value); }
namespace other { int scope(int n) { auto value = api::make(n); return take(value); } }
int receiver(int n) { auto first = api::make(n); auto second = transform(first, n); return second.read(); }
api::Value cycle(api::Value);
int self_reference() { auto value = cycle(value); return take(value); }
int absent() { auto value = unknown(); return take(value); }
int hidden() { int make = 1; auto value = make(); return take(value); }
int conditional(bool flag) { auto value = flag ? api::make(1) : api::make(2); return take(value); }
api::Value conflict(api::Value);
namespace api { Value conflict(Value); }
int ambiguous(api::Value initial) { auto value = conflict(initial); return take(value); }
"#;
    let files = [("api.hpp", header), ("sample.cpp", source)];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    for call in ["chain:take", "copy:take", "other::scope:take"] {
        let target = result[call]
            .as_ref()
            .unwrap_or_else(|| panic!("{call}: {result:#?}"));
        assert_eq!(target.name, "api::take");
        assert_eq!(target.path, "api.hpp");
        assert!(!target.has_body);
    }
    assert_eq!(
        result["receiver:second.read"].as_ref().unwrap().name,
        "api::Value::read"
    );
    for call in [
        "self_reference:cycle",
        "self_reference:take",
        "absent:take",
        "hidden:take",
        "conditional:take",
        "ambiguous:conflict",
        "ambiguous:take",
    ] {
        assert_eq!(result.get(call), Some(&None), "{call}: {result:#?}");
    }
}

#[test]
fn free_calls_combine_ordinary_and_associated_candidates_using_actual_arguments() {
    let source = r#"
namespace remote {
struct Token {};
int take(Token) { return 2; }
int conflict(Token) { return 2; }
int only(Token) { return 2; }
}
int take(int) { return 1; }
int conflict(remote::Token) { return 1; }
int scalar(int) { return 1; }
int exact(remote::Token) { return 1; }
namespace remote { int exact(Token) { return 2; } }
int better(remote::Token value) { return take(value); }
int ambiguous(remote::Token value) { return conflict(value); }
int associated_only(remote::Token value) { return only(value); }
int qualified(remote::Token value) { return ::exact(value); }
struct Caller { int entry(int value) { return scalar(value); } };
namespace nested { int literal() { return scalar(7); } }
int expression(int value) { return scalar(((value /* operand */ - (1)) * 2)); }
namespace overloaded { struct Value {}; Value operator-(Value, int); int scalar(Value); }
int unsupported_expression(overloaded::Value value) { return scalar(value - 1); }
namespace base { struct Base {}; }
namespace derived { struct Value : base::Base {}; }
namespace base { int inherited(derived::Value) { return 3; } }
int via_base(derived::Value value) { return inherited(value); }
struct Member { int take(remote::Token) { return 4; }
    int entry(remote::Token value) { return take(value); } };
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("sample.cpp", source)], scoped);
        for (call, target) in [
            ("better:take", "remote::take"),
            ("associated_only:only", "remote::only"),
            ("qualified:::exact", "exact"),
            ("Caller::entry:scalar", "scalar"),
            ("nested::literal:scalar", "scalar"),
            ("expression:scalar", "scalar"),
            ("via_base:inherited", "base::inherited"),
            ("Member::entry:take", "Member::take"),
        ] {
            assert_eq!(
                calls.get(call).and_then(|v| v.as_deref()),
                Some(target),
                "{call}: {calls:#?}"
            );
        }
        assert_eq!(calls.get("ambiguous:conflict"), Some(&None), "{calls:#?}");
        assert_eq!(
            calls.get("unsupported_expression:scalar"),
            Some(&None),
            "{calls:#?}"
        );
    }
}

#[test]
fn incomplete_associated_lookup_does_not_choose_an_ordinary_target() {
    let source = r#"
namespace hidden { struct Token { friend int pick(Token) { return 2; } }; }
int pick(int) { return 1; }
int hidden_friend(hidden::Token value) { return pick(value); }
namespace harmless {
struct Token { friend int other(Token); };
int choose(Token) { return 3; }
}
int unrelated_friend(harmless::Token value) { return choose(value); }
namespace versioned { inline namespace v1 { struct Token {}; int select(Token); } }
int select(int) { return 1; }
int inline_namespace(versioned::v1::Token value) { return select(value); }
using Unknown = missing::Token;
int unresolved_type(Unknown value) { return pick(value); }
namespace templated { template<class T> struct Box {}; int pick(Box<int>); }
int template_argument(templated::Box<int> value) { return pick(value); }
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("sample.cpp", source)], scoped);
        for call in [
            "hidden_friend:pick",
            "inline_namespace:select",
            "unresolved_type:pick",
        ] {
            assert_eq!(calls.get(call), Some(&None), "{call}: {calls:#?}");
        }
        assert_eq!(
            calls.get("template_argument:pick"),
            Some(&Some("templated::pick".into()))
        );
        assert_eq!(
            calls
                .get("unrelated_friend:choose")
                .and_then(|v| v.as_deref()),
            Some("harmless::choose"),
            "{calls:#?}"
        );
    }
}

#[test]
fn unnamed_template_parameter_recovery_does_not_erase_hiding_or_select_templates() {
    let source = r#"
namespace traits { template<class T> using slot = int; }
namespace sample {
struct Device {
    template<class T, traits::slot<T>* = nullptr> int helper(T) { return 3; }
    int read() { return 7; }
};
struct Base { int read(int) { return 1; } };
struct Hidden : Base {
    template<class T, traits::slot<T>* = nullptr> int read(T) { return 2; }
};
struct Overloaded {
    int read(long) { return 1; }
    template<class T, traits::slot<T>* = nullptr> int read(T) { return 2; }
};
struct Prototype : Base {
    template<class T, traits::slot<T>* = nullptr> int read(T);
};
struct CleanTemplate {
    int read(long) { return 1; }
    template<class T> int read(T) { return 2; }
};
struct Broken {
    template<class T, traits::slot<T>* = nullptr> int helper(T) { return @; }
    int read() { return 7; }
};
int plain(Device* device) { return device->read(); }
int dependent(Device* device) { return device->helper(1); }
int hidden(Hidden* device) { return device->read(1); }
int overloaded(Overloaded* device) { return device->read(1); }
int prototype(Prototype* device) { return device->read(1); }
int clean_template(CleanTemplate* device) { return device->read(1); }
int broken(Broken* device) { return device->read(); }
}
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("sample.cpp", source)], scoped);
        assert_eq!(
            calls
                .get("sample::plain:device->read")
                .and_then(|t| t.as_deref()),
            Some("sample::Device::read"),
            "{calls:#?}"
        );
        for call in [
            "sample::dependent:device->helper",
            "sample::hidden:device->read",
            "sample::overloaded:device->read",
            "sample::prototype:device->read",
            "sample::clean_template:device->read",
            "sample::broken:device->read",
        ] {
            assert_eq!(calls.get(call), Some(&None), "{call}: {calls:#?}");
        }
    }
}

#[test]
fn local_using_limits_stay_inside_their_block_and_after_the_declaration() {
    let header = r#"
namespace api {
int helper() { return 1; }
void configure() { using namespace unavailable; }
struct Device {
    void configure() { using namespace unavailable; }
    int work() { return 2; }
    int invoke() { return work(); }
};
}
"#;
    let source = r#"
#include "api.h"
namespace api {
int outside(Device* value) { return value->work(); }
int before() { helper(); using namespace unavailable; return 0; }
int after() { { using namespace unavailable; } return helper(); }
int inside() { using namespace unavailable; return helper(); }
void descendant() { using namespace unavailable; auto fn = [] { helper(); }; }
int named() { using unavailable::helper; return helper(); }
int unrelated() { using unavailable::other; return helper(); }
int absolute() { using namespace unavailable; return ::api::helper(); }
}
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("api.h", header), ("calls.cpp", source)], scoped);
        let descendant =
            lambda_call_key(source, "void descendant()", "helper();", "api::", "helper");
        for (call, target) in [
            ("api::Device::invoke:work", "api::Device::work"),
            ("api::outside:value->work", "api::Device::work"),
            ("api::before:helper", "api::helper"),
            ("api::after:helper", "api::helper"),
            ("api::unrelated:helper", "api::helper"),
            ("api::absolute:::api::helper", "api::helper"),
        ] {
            assert_eq!(
                calls.get(call).and_then(|t| t.as_deref()),
                Some(target),
                "{call}: {calls:#?}"
            );
        }
        for call in [
            "api::inside:helper",
            descendant.as_str(),
            "api::named:helper",
        ] {
            assert_eq!(calls.get(call), Some(&None), "{call}: {calls:#?}");
        }
    }
}

#[test]
fn class_definitions_do_not_become_free_functions_when_prototypes_are_missing() {
    let source = r#"
struct Missing;
struct Device : Missing { DECLARE_MEMBERS(); };
int Device::read() const { return 1; }
int Device::entry() const { return read(); }
namespace ordinary { int read() { return 2; } int entry() { return read(); } }
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("missing.cpp", source)], scoped);
        assert_eq!(calls.get("Device::entry:read"), Some(&None), "{calls:#?}");
        assert_eq!(
            calls.get("ordinary::entry:read").and_then(|v| v.as_deref()),
            Some("ordinary::read")
        );
    }
}

#[test]
fn unqualified_lookup_searches_bases_and_proven_empty_enclosing_scopes() {
    let header = r#"
namespace api {
int helper() { return 1; }
struct Base {
    int inherited(int value) { return value; }
    static int stable(int value) { return value; }
};
struct Derived : Base { void run(); void captured(); };
namespace nested { int invoke() { return helper(); } }
}
"#;
    let source = r#"
#include "api.h"
namespace api {
void Derived::run() {
    inherited(1);
    this->inherited(2);
    helper();
    auto later = [this] { this->inherited(3); };
    auto independent = [] { stable(4); };
}
void Derived::captured() { auto later = [this] { inherited(5); }; }
}
"#;
    for scoped in [false, true] {
        let calls = analyze_targets(&[("api.h", header), ("calls.cpp", source)], scoped);
        let stable = lambda_call_key(
            source,
            "auto independent =",
            "stable(4);",
            "api::Derived::",
            "stable",
        );
        let captured = lambda_call_key(
            source,
            "void Derived::captured",
            "inherited(5);",
            "api::Derived::",
            "inherited",
        );
        for (call, target) in [
            ("api::Derived::run:inherited", "api::Base::inherited"),
            ("api::Derived::run:this->inherited", "api::Base::inherited"),
            (stable.as_str(), "api::Base::stable"),
            (captured.as_str(), "api::Base::inherited"),
            ("api::Derived::run:helper", "api::helper"),
            ("api::nested::invoke:helper", "api::helper"),
        ] {
            assert_eq!(
                calls.get(call).and_then(|target| target.as_ref()),
                Some(&Target {
                    name: target.into(),
                    path: "api.h".into(),
                    has_body: true,
                }),
                "{call}: {calls:#?}"
            );
        }
    }
}

#[test]
fn outer_lookup_does_not_skip_hiding_missing_bases_or_argument_lookup() {
    let source = r#"
int helper() { return 1; }
int helper(int) { return 2; }
struct Base { int helper; };
struct Field : Base { void run() { helper(); } };
struct Own { int helper; void run() { helper(); } };
struct Overload { int helper(int); };
struct WrongArity : Overload { void run() { helper(); } };
struct Missing;
struct Unknown : Missing { void run() { helper(); } };
struct Imported { using Missing::helper; void run() { helper(); } };
struct NoCaptureBase { int inherited(int) { return 3; } };
struct NoCapture : NoCaptureBase { void run() { auto later = [] { inherited(1); }; } };
struct RvalueBase { int qualified() && { return 5; } };
struct Rvalue : RvalueBase { void run() { qualified(); this->qualified(); } };
namespace imports { using namespace unavailable; void run() { helper(); } }
namespace local { int helper; void run() { helper(); } }
namespace later { void run() { future(); } }
int future() { return 4; }
namespace argument { struct Value { operator int() const; }; int helper(Value); }
struct WithArgument { void run(argument::Value value) { helper(value); } };
template<class helper> struct Dependent { void run() { helper(); } };
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("negative.cpp", source)], scoped);
        let no_capture = lambda_call_key(
            source,
            "struct NoCapture :",
            "inherited(1);",
            "NoCapture::",
            "inherited",
        );
        for call in [
            "Field::run:helper",
            "Own::run:helper",
            "WrongArity::run:helper",
            "Unknown::run:helper",
            "Imported::run:helper",
            no_capture.as_str(),
            "Rvalue::run:qualified",
            "Rvalue::run:this->qualified",
            "imports::run:helper",
            "local::run:helper",
            "later::run:future",
            "Dependent::run:helper",
        ] {
            assert_eq!(calls.get(call), Some(&None), "{call}: {calls:#?}");
        }
        assert_eq!(
            calls
                .get("WithArgument::run:helper")
                .and_then(|v| v.as_deref()),
            Some("argument::helper"),
            "{calls:#?}"
        );
    }
}

#[test]
fn named_using_preserves_unrelated_types_and_members_without_guessing_imports() {
    let header = r#"
namespace api {
class EXPORT Broken : public Missing { using Missing::imported; };
struct Base { int inherited() { return 1; } int imported() { return 2; } };
struct Device : Base {
    using Base::imported;
    int own() { return 3; }
    int imported(int value) { return value; }
};
}
struct Device { int run() { return 4; } };
namespace local { using unavailable::Device; }
namespace blocked { using namespace unavailable; }
namespace global_type { struct Child { int run() { return 5; } }; }
namespace shadowed { using unavailable::global_type; }
"#;
    let source = r#"
#include "types.h"
int own(api::Device* value) { return value->own(); }
int inherited(api::Device* value) { return value->inherited(); }
int imported(api::Device* value) { return value->imported(1); }
namespace local { int invoke(Device* value) { return value->run(); } }
namespace blocked { int invoke(Device* value) { return value->run(); } }
namespace shadowed { int invoke(global_type::Child* value) { return value->run(); } }
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("types.h", header), ("calls.cpp", source)], scoped);
        for (call, target) in [
            ("own:value->own", "api::Device::own"),
            ("inherited:value->inherited", "api::Base::inherited"),
        ] {
            assert_eq!(
                calls.get(call).and_then(|v| v.as_deref()),
                Some(target),
                "{calls:#?}"
            );
        }
        for call in [
            "imported:value->imported",
            "local::invoke:value->run",
            "blocked::invoke:value->run",
            "shadowed::invoke:value->run",
        ] {
            assert_eq!(calls.get(call), Some(&None), "{call}: {calls:#?}");
        }
    }
}

#[test]
fn implicit_member_lookup_preserves_inherited_virtual_dispatch() {
    let source = r#"
template<class T> struct Payload {};
struct Base {
    virtual int send(const Payload<int>& value) { return 1; }
    virtual int plain(int value) { return 1; }
};
struct Derived : Base {
    int send(const Payload<int>& value) { return 2; }
    int plain(int value) { return 2; }
    void run(const Payload<int>& value) { send(value); this->send(value); plain(1); }
};
struct Further : Derived {
    int send(const Payload<int>& value) { return 3; }
    int plain(int value) { return 3; }
};
struct Missing;
struct Partial : Missing {
    void send(const Payload<int>& value) {}
    static void stable(const Payload<int>& value) {}
    void run(const Payload<int>& value) { send(value); stable(value); }
};
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("dispatch.cpp", source)], scoped);
        for call in [
            "Derived::run:send",
            "Derived::run:this->send",
            "Derived::run:plain",
            "Partial::run:send",
        ] {
            assert_eq!(calls.get(call), Some(&None), "{call}: {calls:#?}");
        }
        assert_eq!(
            calls.get("Partial::run:stable").and_then(|t| t.as_deref()),
            Some("Partial::stable")
        );
    }
}

#[test]
fn member_parameter_templates_do_not_require_adl_or_receiver_instantiation() {
    let header = r#"
namespace api {
template<class T> struct Payload {};
struct Device {
    void send(const Payload<int>& value);
    void run(const Payload<int>& value) { send(value); this->send(value); }
    void again(const Payload<int>& value);
    void ambiguous(const Payload<int>& value);
    void ambiguous(const Payload<float>& value);
    virtual void dispatch(const Payload<int>& value);
    template<class T> void dependent(const Payload<T>& value);
};
template<class T> struct Pointer { T* operator->() const { return nullptr; } };
void free_call(const Payload<int>& value);
}
"#;
    let source = r#"
#include "api.h"
namespace api {
void Device::send(const Payload<int>& argument) {}
void Device::again(const Payload<int>& value) { send(value); }
void invoke(Device& device, Pointer<Device> pointer, const Payload<int>& value) {
    device.send(value);
    pointer -> send(value);
    device.ambiguous(value);
    device.dispatch(value);
    device.dependent(value);
    free_call(value);
}
}
"#;
    for scoped in [false, true] {
        let calls = analyze_targets(&[("api.h", header), ("calls.cpp", source)], scoped);
        for call in [
            "api::Device::run:send",
            "api::Device::run:this->send",
            "api::Device::again:send",
            "api::invoke:device.send",
            "api::invoke:pointer -> send",
        ] {
            assert_eq!(
                calls.get(call).and_then(|target| target.as_ref()),
                Some(&Target {
                    name: "api::Device::send".into(),
                    path: "calls.cpp".into(),
                    has_body: true,
                }),
                "{call}: {calls:#?}"
            );
        }
        for call in [
            "api::invoke:device.ambiguous",
            "api::invoke:device.dispatch",
            "api::invoke:device.dependent",
        ] {
            assert_eq!(calls.get(call), Some(&None), "{call}: {calls:#?}");
        }
        assert_eq!(
            calls
                .get("api::invoke:free_call")
                .and_then(|target| target.as_ref()),
            Some(&Target {
                name: "api::free_call".into(),
                path: "api.h".into(),
                has_body: false
            })
        );
    }
}

#[test]
fn arrow_uses_actual_operator_return_and_named_template_parameter() {
    let header = r#"
namespace api {
template<class T> struct List {};
struct Device {
    void unrelated(const List<int>& values = {});
    int run() { return 1; }
};
struct Other { int run() { return 2; } };
template<class Ignored, class Value> struct Handle {
    Value* operator->() const { return nullptr; }
    void housekeeping();
};
template<class Ignored, class Value> void Handle<Ignored, Value>::housekeeping() {}
Handle<Other, Device> make();
struct Fixed { Other* operator->() { return nullptr; } };
struct Missing;
template<class T> struct Pointer { T* operator->() const { return nullptr; } };
}
"#;
    let source = r#"
#include "api.hpp"
namespace extensions {}
namespace api {
using namespace extensions;
Handle<Other, Device> make() { return {}; }
}
namespace caller {
struct Device { int run() { return 3; } };
int from_factory() { auto handle = api::make(); return handle->run(); }
int explicit_type(api::Handle<api::Other, api::Device> handle) { return handle->run(); }
int fixed(api::Fixed handle) { return handle->run(); }
int missing(api::Pointer<api::Missing> handle) { return handle->run(); }
}
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("api.hpp", header), ("use.cpp", source)], scoped);
        for (call, target) in [
            ("caller::from_factory:handle->run", Some("api::Device::run")),
            (
                "caller::explicit_type:handle->run",
                Some("api::Device::run"),
            ),
            ("caller::fixed:handle->run", Some("api::Other::run")),
            ("caller::missing:handle->run", None),
        ] {
            assert_eq!(
                calls.get(call).map(|target| target.as_deref()),
                Some(target),
                "{call}: {calls:#?}"
            );
        }
    }
}

#[test]
fn arrow_preserves_operator_without_overwriting_or_resolving_missing_member() {
    let source = r#"
struct Device { int run() { return 1; } };
struct Missing;
template<class T> struct Pointer { T* operator->() const { return nullptr; } };
int connected(Pointer<Device> p) { return p->run(); }
int broken(Pointer<Missing> p) { return p->run(); }
"#;
    for scoped in [false, true] {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let file = FileId::generate("arrow.cpp");
        let facts = extract_file_with_mode(
            &create_frontend(Language::Cpp).unwrap(),
            file,
            Path::new("arrow.cpp"),
            source,
            "fixture",
            ExtractionMode::Structural,
            &(),
        )
        .unwrap();
        store.insert_file_facts(&facts).unwrap();
        let mut resolver = ReferenceResolver::new(store.clone());
        let (pairs, stats) = if scoped {
            store.insert_closure_generation("arrow").unwrap();
            resolver
                .resolve_for_closure("arrow", 0, &[file], None)
                .unwrap()
        } else {
            resolver
                .resolve_all_parallel(store.clone(), None, None)
                .unwrap()
        };
        assert_eq!(stats.resolved + stats.unresolved, stats.total_refs);
        for name in ["connected", "broken"] {
            let caller = facts.symbols.iter().find(|s| s.name == name).unwrap();
            let call = facts
                .references
                .iter()
                .find(|r| r.source_symbol == Some(caller.id) && r.kind == ReferenceKind::Call)
                .unwrap();
            let edges: Vec<_> = pairs.iter().filter(|(r, _)| r.id == call.id).collect();
            assert_eq!(
                edges.len(),
                if name == "connected" { 2 } else { 1 },
                "{name}: {edges:?}"
            );
            let implicit = edges
                .iter()
                .find(|(_, t)| t.strategy == types::ResolutionStrategy::ImplicitOperator)
                .unwrap();
            let operator = store
                .find_symbol_by_id(&implicit.1.symbol_id)
                .unwrap()
                .unwrap();
            assert_eq!(operator.qualified_name, "Pointer::operator->");
            assert_ne!(operator.range, operator.name_range);
            assert_eq!(implicit.0.range, call.range);
            if !scoped {
                let saved = store
                    .find_references_by_file(&file)
                    .unwrap()
                    .into_iter()
                    .find(|r| r.id == call.id)
                    .unwrap();
                if name == "broken" {
                    assert!(saved.resolved.is_none());
                } else {
                    let target = store
                        .find_symbol_by_id(&saved.resolved.unwrap().symbol_id)
                        .unwrap()
                        .unwrap();
                    assert_eq!(target.qualified_name, "Device::run");
                }
            }
        }
    }
}

#[test]
fn arrow_does_not_apply_primary_templates_over_specializations_or_unknown_operators() {
    let source = r#"
namespace api {
struct Device { int run() { return 1; } };
struct Other { int run() { return 2; } };
template<class T> struct Specialized { T* operator->() const { return nullptr; } };
template<> struct Specialized<Device> { Other* operator->() const { return nullptr; } };
template<class T> struct Partial { T* operator->() const { return nullptr; } };
template<class T> struct Partial<const T> { Other* operator->() const { return nullptr; } };
template<class T> struct NoOperator {};
template<class T> struct ProxyReturn { NoOperator<T> operator->(); };
template<class T> struct CvReturn { const T* operator->(); };
template<class T> struct Overloaded { T* operator->(); Other* operator->() const; };
template<class T> struct MemberSpecialized { T* operator->() const { return nullptr; } };
template<> Device* MemberSpecialized<Device>::operator->() const { return nullptr; }
int specialized(Specialized<Device> p) { return p->run(); }
int partial(Partial<Device> p) { return p->run(); }
int absent(NoOperator<Device> p) { return p->run(); }
int proxy(ProxyReturn<Device> p) { return p->run(); }
int cv(CvReturn<Device> p) { return p->run(); }
int overloaded(Overloaded<Device> p) { return p->run(); }
int member_specialized(MemberSpecialized<Device> p) { return p->run(); }
}
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("arrow.cpp", source)], scoped);
        for name in [
            "specialized",
            "partial",
            "absent",
            "proxy",
            "cv",
            "overloaded",
            "member_specialized",
        ] {
            assert_eq!(
                calls.get(&format!("api::{name}:p->run")),
                Some(&None),
                "{calls:#?}"
            );
        }
    }
}

#[test]
fn cpp_targets_respect_receivers_qualified_scopes_arity_and_ambiguity() {
    let source = r#"
namespace left { class Tool { public: static int run(int value) { return value; } }; }
namespace right { class Tool { public: static int run(int value) { return -value; } }; }
namespace demo {
class Driver { public: Driver() {} Driver* other; int step(int value); };
int Driver::step(int value) { other->step(value); return step(value); }
int recurse(int value) { return recurse(value - 1); }
int zero() { return 0; }
int overloaded(int value) { return value; }
int overloaded(double value) { return 0; }
int callback(int value) { return value; }
int shadow(int callback) { return callback(1); }
int shadow_pointer(int (*callback)(int)) { return callback(1); }
int inherited(int value) { return value; }
class Base { public: int inherited(int value) { return value; } };
class Child : public Base { public: int call(int value) { return inherited(value); } };
template<class T> int templated(T value) { return value; }
int caller() {
    left::Tool::run(1);
    right::Tool::run(1);
    Ghost::run(1);
    zero(1);
    overloaded(1);
    templated(1);
    return 0;
}
}
class Unrelated { public: int step(int value) { return value; } };
"#;
    let global = analyze(&[("main.cpp", source)], false);
    assert_eq!(global, analyze(&[("main.cpp", source)], true));
    for (call, target) in [
        ("demo::Driver::step:other->step", Some("demo::Driver::step")),
        ("demo::Driver::step:step", Some("demo::Driver::step")),
        ("demo::recurse:recurse", Some("demo::recurse")),
        ("demo::caller:left::Tool::run", Some("left::Tool::run")),
        ("demo::caller:right::Tool::run", Some("right::Tool::run")),
        ("demo::caller:Ghost::run", None),
        ("demo::caller:zero", None),
        ("demo::caller:overloaded", Some("demo::overloaded")),
        ("demo::shadow:callback", None),
        ("demo::shadow_pointer:callback", None),
        ("demo::Child::call:inherited", Some("demo::Base::inherited")),
        ("demo::caller:templated", None),
    ] {
        assert_eq!(
            global.get(call).map(|target| target.as_deref()),
            Some(target),
            "{call}: {global:#?}"
        );
    }
}

#[test]
fn declared_object_pointer_reference_and_field_receivers_use_visible_record_members() {
    let header = r#"
namespace api {
struct Device { int run(int value); };
struct Driver { Device* device; int apply(int value); };
}
namespace unrelated { using Alias = int; }
"#;
    let body = r#"
#include "api.hpp"
int api::Device::run(int value) { return value; }
int api::Driver::apply(int value) { return device->run(value); }
int pointer(api::Device* device) { return device->run(1); }
int reference(api::Device& device) { return device.run(2); }
int object(api::Device device) { return device.run(3); }
int local() { api::Device device; return device.run(4); }
namespace api { Device* global_device; int global_call() { return global_device->run(1); } }
int nested(api::Device* device) { { int device = 0; device->run(5); } return device->run(6); }
"#;
    let noise = "struct Device { int run(int value) { return value; } }; namespace unrelated { struct Device { int run(int value) { return value; } }; }";
    let files = [
        ("api.hpp", header),
        ("main.cpp", body),
        ("noise.cpp", noise),
    ];
    let result = analyze(&files, false);
    assert_eq!(result, analyze(&files, true));
    for call in [
        "api::Driver::apply:device->run",
        "pointer:device->run",
        "reference:device.run",
        "object:device.run",
        "local:device.run",
        "api::global_call:global_device->run",
        "nested:device->run",
    ] {
        assert_eq!(
            result[call].as_deref(),
            Some("api::Device::run"),
            "{call}: {result:#?}"
        );
    }
    let missing = analyze(&[("main.cpp", body), ("noise.cpp", noise)], false);
    for target in missing.values() {
        assert_eq!(target, &None);
    }
}

#[test]
fn unsupported_receiver_semantics_do_not_choose_same_name_methods() {
    let source = r#"
struct Device { int run() { return 1; } };
struct Virtual { virtual int run() { return 1; } };
template<class T> struct Handle { T* operator->(); };
struct Overload { int run() { return 1; } int run() const { return 2; } };
struct Rvalue { int run() && { return 1; } };
int smart(Handle<Device> device) { return device->run(); }
int unknown(auto device) { return device->run(); }
int constant(const Device* device) { return device->run(); }
int dynamic(Virtual* device) { return device->run(); }
int overload(Overload* device) { return device->run(); }
int category(Rvalue* device) { return device->run(); }
int wrong_operator(Device device) { return device->run(); }
int capture(Device* device) { return [device] { return device->run(); }(); }
int shadow(Device* device) { { int device = 0; return device->run(); } }
int type_shadow(int Device) { Device* device; return device->run(); }
struct Driver {
    Device* device;
    int deep(Device** device) { return device->run(); }
    int array(Device device[2]) { return device->run(); }
    int callback(Device* (*device)()) { return device->run(); }
    int destructure() { auto [device, other] = make_pair(); return device->run(); }
    int prototype() { Device* device(); return device->run(); }
};
struct EnumOwner { enum Device { Off }; Device* device; int run() { return device->run(); } };
namespace hidden { struct Device; Device* d; int call() { return d->run(); } }
"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
    for (call, target) in &result {
        if call == "smart:device->run"
            || call == &lambda_call_key(source, "int capture(", "device->run", "", "device->run")
        {
            assert_eq!(target.as_deref(), Some("Device::run"));
            continue;
        }
        if call == "capture:[device] { return device->run(); }" {
            assert_eq!(
                target.as_deref(),
                Some(format!("<lambda@{}>", source.find("[device]").unwrap()).as_str())
            );
            continue;
        }
        assert_eq!(target, &None, "{call}: {result:#?}");
    }
    let alias = "struct Device { int run() { return 1; } }; namespace local { using Device = int; int call(Device* d) { return d->run(); } }";
    assert_eq!(
        analyze(&[("alias.cpp", alias)], false)["local::call:d->run"],
        None
    );
    let header =
        "struct Device { int run() { return 1; } }; namespace local { using Device = int; }";
    let source =
        "#include \"alias.hpp\"\n namespace local { int call(Device* d) { return d->run(); } }";
    let files = [("alias.hpp", header), ("main.cpp", source)];
    let result = analyze(&files, false);
    assert_eq!(result, analyze(&files, true));
    assert_eq!(result["local::call:d->run"], None);
    let header = "namespace other { struct Device {}; } namespace local { using namespace other; } struct Device { int run() { return 1; } };";
    let result = analyze(&[("alias.hpp", header), ("main.cpp", source)], false);
    assert_eq!(result["local::call:d->run"], None);
}

#[test]
fn cpp_visible_declarations_link_to_unique_bodies_without_directory_tie_breaking() {
    let header = "namespace api { int process(int value); class Gate { public: static int check(int mode = 0); }; }";
    let implementation = "namespace api { int process(int value) { return value; } int Gate::check(int mode) { return mode; } }";
    let caller =
        "#include \"api.hpp\"\nint call() { return api::process(1) + api::Gate::check(); }";
    let unrelated = "namespace other { int process(int value) { return -value; } int Gate::check(int mode) { return -mode; } }";
    let mut baseline = None;
    for unrelated_path in ["near.cpp", "distant/other.cpp"] {
        let files = [
            ("api.hpp", header),
            ("impl/source.cpp", implementation),
            ("main.cpp", caller),
            (unrelated_path, unrelated),
        ];
        let result = analyze(&files, false);
        assert_eq!(result, analyze(&files, true));
        assert_eq!(
            result.get("call:api::process").unwrap().as_deref(),
            Some("api::process")
        );
        assert_eq!(
            result.get("call:api::Gate::check").unwrap().as_deref(),
            Some("api::Gate::check")
        );
        if let Some(baseline) = &baseline {
            assert_eq!(&result, baseline);
        }
        baseline = Some(result);
    }
    let missing = analyze(
        &[("main.cpp", caller), ("impl/source.cpp", implementation)],
        false,
    );
    assert_eq!(missing.get("call:api::process"), Some(&None));
    assert_eq!(missing.get("call:api::Gate::check"), Some(&None));
}

#[test]
fn qualified_ordinary_functions_accept_nested_template_parameter_types_without_guessing_overloads()
{
    let header = r#"
template<class T> struct Box {};
template<class A, class B> struct Pair {};
namespace api {
void accept(const Box<Pair<int, Box<double>>>& value, int mode = 0);
void overloaded(Box<int> value);
void overloaded(Box<double> value);
void callback(void (*fn)(int));
}

"#;
    let body = "#include \"api.hpp\"\nnamespace api { void accept(const Box<Pair<int, Box<double>>>& value, int mode) {} }";
    let source = r#"#include "api.hpp"
void entry(Box<Pair<int, Box<double>>> value, Box<int> other) {
    api::accept(value);
    api::overloaded(other);
    api::callback(nullptr);
}
namespace api { void unqualified(Box<Pair<int, Box<double>>> value) { accept(value); } }
"#;
    let files = [
        ("api.hpp", header),
        ("body.cpp", body),
        ("main.cpp", source),
        (
            "noise.cpp",
            "namespace other { void accept(Box<int> value) {} }",
        ),
    ];
    let result = analyze(&files, false);
    assert_eq!(result, analyze(&files, true));
    assert_eq!(result["entry:api::accept"].as_deref(), Some("api::accept"));
    assert_eq!(result["entry:api::overloaded"], None);
    assert_eq!(result["entry:api::callback"], None);
    assert_eq!(
        result["api::unqualified:accept"].as_deref(),
        Some("api::accept")
    );
    let missing_header = analyze(&[("main.cpp", source), ("body.cpp", body)], false);
    assert_eq!(missing_header["entry:api::accept"], None);
}

#[test]
fn ast_parameter_identity_reaches_bodies_with_renamed_and_unnamed_parameters() {
    let header = r#"
namespace api {
struct Node {};
struct Reader { bool read(const Node& nodePtr); };
int unnamed(const int*, double&&);
int qualified(const int value, int* const pointer);
int defaults(int mode = (1 + 2));
void repeated(int first);
void repeated(int renamed);
}
"#;
    let body = r#"#include "api.hpp"
namespace api {
bool Reader::read(Node const& nodeptr) { return true; }
int unnamed(const int* value, double&& other) { return 1; }
int qualified(int renamed, int* other) { return renamed; }
int defaults(int value) { return value; }
void repeated(int value) {}
}
"#;
    let caller = r#"#include "api.hpp"
void entry(api::Reader* reader, api::Node node) {
    reader->read(node);
    api::unnamed(nullptr, 1.0);
    api::qualified(1, nullptr);
    api::defaults();
    api::repeated(1);
}
"#;
    let files = [
        ("api.hpp", header),
        ("body.cpp", body),
        ("main.cpp", caller),
    ];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    for (call, name) in [
        ("entry:reader->read", "api::Reader::read"),
        ("entry:api::unnamed", "api::unnamed"),
        ("entry:api::qualified", "api::qualified"),
        ("entry:api::defaults", "api::defaults"),
        ("entry:api::repeated", "api::repeated"),
    ] {
        assert_eq!(
            result[call],
            Some(Target {
                name: name.into(),
                path: "body.cpp".into(),
                has_body: true
            }),
            "{call}: {result:#?}"
        );
    }
    let missing = analyze_targets(&[("api.hpp", header), ("main.cpp", caller)], false);
    assert_eq!(
        missing["entry:reader->read"],
        Some(Target {
            name: "api::Reader::read".into(),
            path: "api.hpp".into(),
            has_body: false
        })
    );
}

#[test]
fn ast_parameter_identity_preserves_type_overloads_and_unsupported_declarators() {
    let header = r#"
namespace api {
int ref(int& original);
int ref(int&& renamed);
int pointee(const int* original);
int pointee(int* renamed);
int layers(int* const* original);
int layers(int** renamed);
int callback(void (*fn)(int));
int array(int values[2]);
int variadic(int first, ...);
struct Cv { int run() const; int run(); };
struct Ref { int run() &; int run() &&; };
}
"#;
    let caller = r#"#include "api.hpp"
void entry(api::Cv* cv, api::Ref* ref) {
    api::ref(1); api::pointee(nullptr); api::layers(nullptr);
    api::callback(nullptr); api::array(nullptr); api::variadic(1);
    cv->run(); ref->run();
}
"#;
    let files = [("api.hpp", header), ("main.cpp", caller)];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    assert_eq!(result.len(), 8);
    for (call, target) in result {
        assert_eq!(target, None, "{call}");
    }
}

#[test]
fn renamed_nested_template_parameters_associate_only_the_matching_body() {
    let header = r#"
template<class T> struct Box {};
namespace api { void accept(const Box<Box<int>>& original, int mode = 0); }
"#;
    let body = r#"#include "api.hpp"
namespace api {
void accept(Box<Box<int>> const& renamed, int other) {}
void accept(Box<Box<double>> const& renamed, int other) {}
}
"#;
    let source = "#include \"api.hpp\"\nvoid entry() { api::accept({}); }";
    let files = [
        ("api.hpp", header),
        ("body.cpp", body),
        ("main.cpp", source),
    ];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    assert_eq!(
        result["entry:api::accept"],
        Some(Target {
            name: "api::accept".into(),
            path: "body.cpp".into(),
            has_body: true
        })
    );
    let duplicates = [
        ("api.hpp", header),
        ("body.cpp", body),
        ("main.cpp", source),
        ("duplicate.cpp", body),
    ];
    assert_eq!(
        analyze_targets(&duplicates, false)["entry:api::accept"],
        None
    );
}

#[test]
fn same_written_named_type_in_unrelated_translation_units_does_not_prove_body_identity() {
    let header = "namespace api { namespace { using Value = int; } void accept(Value original); }";
    let unrelated =
        "namespace api { namespace { using Value = double; } void accept(Value renamed) {} }";
    let caller = "#include \"api.hpp\"\nvoid entry() { api::accept(1); }";
    let files = [
        ("api.hpp", header),
        ("unrelated.cpp", unrelated),
        ("main.cpp", caller),
    ];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    assert_eq!(
        result["entry:api::accept"],
        Some(Target {
            name: "api::accept".into(),
            path: "api.hpp".into(),
            has_body: false
        })
    );
}

#[test]
fn file_local_functions_do_not_link_to_another_translation_units_body() {
    let header = "static int local(int original);";
    let other = "#include \"api.hpp\"\nstatic int local(int renamed) { return renamed; }";
    let caller = "#include \"api.hpp\"\nint entry() { return local(1); }";
    let files = [
        ("api.hpp", header),
        ("other.cpp", other),
        ("main.cpp", caller),
    ];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    assert_eq!(
        result["entry:local"],
        Some(Target {
            name: "local".into(),
            path: "api.hpp".into(),
            has_body: false
        })
    );
    let own = "#include \"api.hpp\"\nstatic int local(int renamed) { return renamed; } int entry() { return local(1); }";
    let files = [("api.hpp", header), ("other.cpp", other), ("main.cpp", own)];
    assert_eq!(
        analyze_targets(&files, false)["entry:local"],
        Some(Target {
            name: "local".into(),
            path: "main.cpp".into(),
            has_body: true
        })
    );
}

#[test]
fn unnamed_namespace_functions_keep_local_bodies_and_exclude_remote_ones() {
    let main = "namespace api { namespace { int helper(int value); int defined(int value) { return value; } } int entry() { return helper(1) + defined(2); } }";
    let other = "namespace api { namespace { int helper(int value) { return value; } int defined(int value) { return value; } } }";
    let files = [("main.cpp", main), ("other.cpp", other)];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    assert_eq!(
        result["api::entry:helper"],
        Some(Target {
            name: "api::helper".into(),
            path: "main.cpp".into(),
            has_body: false
        })
    );
    assert_eq!(
        result["api::entry:defined"],
        Some(Target {
            name: "api::defined".into(),
            path: "main.cpp".into(),
            has_body: true
        })
    );
}

#[test]
fn qualified_nested_template_argument_syntax_keeps_declaration_body_association() {
    let header = r#"
namespace lib { template<class T> struct Box {}; template<class A, class B> struct Pair {}; }
namespace api { void accept(const lib::Box<lib::Pair<lib::Box<int>, lib::Box<double>>>& original); }
"#;
    let body = r#"#include "api.hpp"
namespace api { void accept(lib::Box<lib::Pair<lib::Box<int>, lib::Box<double>>> const& renamed) {} }
"#;
    let caller = "#include \"api.hpp\"\nvoid entry() { api::accept({}); }";
    let files = [
        ("api.hpp", header),
        ("body.cpp", body),
        ("main.cpp", caller),
    ];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    assert_eq!(
        result["entry:api::accept"],
        Some(Target {
            name: "api::accept".into(),
            path: "body.cpp".into(),
            has_body: true
        })
    );
}

#[test]
fn body_association_follows_resolved_header_chain_without_assuming_missing_includes() {
    let api = "namespace api { struct Value {}; void accept(const Value& original); }";
    let wrapper = "#include \"api.hpp\"\n#include \"cycle.hpp\"";
    let cycle = "#include \"wrapper.hpp\"";
    let body = "#include \"wrapper.hpp\"\nnamespace api { void accept(Value const& renamed) {} }";
    let caller = "#include \"api.hpp\"\nvoid entry() { api::accept({}); }";
    let files = [
        ("api.hpp", api),
        ("wrapper.hpp", wrapper),
        ("cycle.hpp", cycle),
        ("body.cpp", body),
        ("main.cpp", caller),
    ];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    assert_eq!(
        result["entry:api::accept"],
        Some(Target {
            name: "api::accept".into(),
            path: "body.cpp".into(),
            has_body: true
        })
    );
    let missing = [
        ("api.hpp", api),
        ("cycle.hpp", cycle),
        ("body.cpp", body),
        ("main.cpp", caller),
    ];
    assert_eq!(
        analyze_targets(&missing, false)["entry:api::accept"],
        Some(Target {
            name: "api::accept".into(),
            path: "api.hpp".into(),
            has_body: false
        })
    );
}

#[test]
fn inherited_members_require_complete_bases_and_preserve_hiding_and_virtual_dispatch() {
    let base = r#"
namespace api {
struct Base;
struct Base { int run(int input); virtual int dynamic(); };
struct Other { int run(int value); };
struct Empty {};
}
"#;
    let header = r#"#include "base.hpp"
namespace api {
struct Mid : Base {};
struct Leaf : Mid { int own(int value); int dynamic(); };
struct Hidden : Base { int run; };
struct Overload : Base { int run(); };
struct Joined : Mid, Other {};
struct Left : Base {};
struct Diamond : Left, Mid {};
struct WithEmpty : Mid, Empty {};
struct VirtualBase : virtual Base {};
struct Missing : Absent { int run(int value); };
struct Using : Base { using Base::run; int run(); };
}
"#;
    let body = r#"#include "api.hpp"
int api::Base::run(int renamed) { return renamed; }
int api::Leaf::own(int input) { return input; }
int inherited(api::Leaf* v) { return v->run(1); }
int own(api::Leaf* v) { return v->own(1); }
int overridden(api::Leaf* v) { return v->dynamic(); }
int hidden(api::Hidden* v) { return v->run(1); }
int wrong_arity(api::Overload* v) { return v->run(1); }
int joined(api::Joined* v) { return v->run(1); }
int diamond(api::Diamond* v) { return v->run(1); }
int unique(api::WithEmpty* v) { return v->run(1); }
int virtual_base(api::VirtualBase* v) { return v->run(1); }
int missing(api::Missing* v) { return v->run(1); }
int using_name(api::Using* v) { return v->run(1); }
"#;
    let noise = "namespace api { struct Absent {}; }";
    let files = [
        ("base.hpp", base),
        ("api.hpp", header),
        ("body.cpp", body),
        ("noise.cpp", noise),
    ];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    for (call, name) in [
        ("inherited:v->run", "api::Base::run"),
        ("own:v->own", "api::Leaf::own"),
        ("unique:v->run", "api::Base::run"),
    ] {
        let target = result[call]
            .as_ref()
            .unwrap_or_else(|| panic!("{call}: {result:#?}"));
        assert_eq!(target.name, name);
        assert_eq!(target.path, "body.cpp");
        assert!(target.has_body);
    }
    for call in [
        "overridden:v->dynamic",
        "hidden:v->run",
        "wrong_arity:v->run",
        "joined:v->run",
        "diamond:v->run",
        "virtual_base:v->run",
        "missing:v->run",
        "using_name:v->run",
    ] {
        assert_eq!(result[call], None, "{call}: {result:#?}");
    }
    let missing = analyze_targets(
        &[
            ("api.hpp", header),
            ("body.cpp", body),
            ("noise.cpp", noise),
        ],
        false,
    );
    for (call, target) in missing {
        assert_eq!(target, None, "{call}");
    }
}

#[test]
fn auto_factory_receivers_use_declared_return_type_without_guessing_runtime_values() {
    let header = r#"
namespace api {
struct Base { int run(int value); };
struct Device : Base {};
struct Virtual { virtual int run(int value); };
struct Factory { Device* create(); int apply(); };
Device make();
Device* make_pointer();
const Device* make_constant();
const Device& make_reference();
Virtual* make_virtual();
Device* overloaded(int value);
Device* overloaded(double value);
}
"#;
    let body = r#"#include "api.hpp"
namespace api {
int Base::run(int input) { return input; }
Device make() { return {}; }
Device* make_pointer() { return nullptr; }
const Device* make_constant() { return nullptr; }
const Device& make_reference() { static Device device; return device; }
Virtual* make_virtual() { return nullptr; }
Device* Factory::create() { return nullptr; }
int Factory::apply() { auto d = create(); return d->run(1); }
}
int object() { auto d = api::make(); return d.run(1); }
int pointer() { auto d = api::make_pointer(); return d->run(1); }
int copy_reference() { auto d = api::make_reference(); return d.run(1); }
int constant() { auto d = api::make_constant(); return d->run(1); }
int dynamic() { auto d = api::make_virtual(); return d->run(1); }
int overload() { auto d = api::overloaded(1); return d->run(1); }
int missing() { auto d = api::unavailable(); return d->run(1); }
int shadowed() { auto create = 1; auto d = create(); return d->run(1); }
int conditional(bool b) { auto d = b ? api::make_pointer() : nullptr; return d->run(1); }
int deduced_return() { auto factory = [] { return api::make_pointer(); }; auto d = factory(); return d->run(1); }
"#;
    let files = [("api.hpp", header), ("body.cpp", body)];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    for call in [
        "api::Factory::apply:d->run",
        "object:d.run",
        "pointer:d->run",
        "copy_reference:d.run",
    ] {
        let target = result[call]
            .as_ref()
            .unwrap_or_else(|| panic!("{call}: {result:#?}"));
        assert_eq!(target.name, "api::Base::run");
        assert_eq!(target.path, "body.cpp");
        assert!(target.has_body);
    }
    for call in [
        "constant:d->run",
        "dynamic:d->run",
        "overload:d->run",
        "missing:d->run",
        "shadowed:d->run",
        "conditional:d->run",
        "deduced_return:d->run",
    ] {
        assert_eq!(result[call], None, "{call}: {result:#?}");
    }
}

#[test]
fn auto_member_factory_results_reuse_receiver_and_argument_resolution() {
    let header = r#"
namespace api {
struct Item { int read() { return 1; } };
struct Factory {
    Item* make();
    Item* create(int value);
    const Item& borrow();
    Item* choose(int value);
    Item* choose(double value);
    virtual Item* dynamic();
};
struct Derived : Factory {};
Factory* factory();
struct Owner { Factory* source; int read(); };
}
namespace noise { struct Item { int read() { return 2; } }; }
"#;
    let body = r#"#include "api.hpp"
int pointer(api::Factory* source) { auto value = source->make(); return value->read(); }
int object(api::Factory& source) { auto value = source.make(); return value->read(); }
int inherited(api::Derived* source) { auto value = source->make(); return value->read(); }
int argument(api::Factory* source) { auto value = source->create(1); return value->read(); }
int copy(api::Factory* source) { auto value = source->borrow(); return value.read(); }
int chain() { auto source = api::factory(); auto value = source->make(); return value->read(); }
int api::Owner::read() { auto value = source->make(); return value->read(); }
int overload(api::Factory* source) { auto value = source->choose(1); return value->read(); }
int dynamic(api::Factory* source) { auto value = source->dynamic(); return value->read(); }
int conditional(bool flag, api::Factory* first, api::Factory* second) {
    auto value = (flag ? first : second)->make(); return value->read();
}
"#;
    let files = [("api.hpp", header), ("main.cpp", body)];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    for call in [
        "pointer:value->read",
        "object:value->read",
        "inherited:value->read",
        "argument:value->read",
        "copy:value.read",
        "chain:value->read",
        "api::Owner::read:value->read",
    ] {
        let target = result[call]
            .as_ref()
            .unwrap_or_else(|| panic!("{call}: {result:#?}"));
        assert_eq!(target.name, "api::Item::read", "{call}");
        assert_eq!(target.path, "api.hpp", "{call}");
        assert!(target.has_body, "{call}");
    }
    // Overload ranking, virtual factory selection and conditional receivers
    // retain their existing unsupported status, not a guessed return type.
    for call in [
        "overload:value->read",
        "dynamic:value->read",
        "conditional:value->read",
    ] {
        assert_eq!(result[call], None, "{call}: {result:#?}");
    }
    // No factory definition is needed, and the recorded declaration remains
    // separate from the body reached after using its declared return type.
    for call in ["pointer:source->make", "object:source.make"] {
        assert!(!result[call].as_ref().unwrap().has_body, "{call}");
    }
}

#[test]
fn member_factory_auto_chains_keep_a_shared_deduction_limit() {
    let header = "struct Item { int read(); }; struct Factory { Factory* next(); Item* make(); };";
    let mut body = String::from(
        "#include \"api.hpp\"\nint long_chain(Factory* start) { auto link0 = start->next();\n",
    );
    for i in 1..96 {
        body.push_str(&format!("auto link{i} = link{}->next();\n", i - 1));
    }
    body.push_str("auto value = link95->make(); return value->read(); }");
    let files = [("api.hpp", header), ("main.cpp", body.as_str())];
    let result = analyze(&files, false);
    assert_eq!(result, analyze(&files, true));
    assert_eq!(
        result["long_chain:start->next"].as_deref(),
        Some("Factory::next")
    );
    assert_eq!(
        result["long_chain:link0->next"].as_deref(),
        Some("Factory::next")
    );
    assert_eq!(result["long_chain:value->read"], None);
}

#[test]
fn factory_declaration_supplies_static_type_even_without_its_body() {
    let header = "namespace api { struct Device { int run() { return 1; } }; Device* make(); const Device& borrow(); }";
    let caller = "#include \"api.hpp\"\nint pointer() { auto p = api::make(); return p->run(); } int copy() { auto p = api::borrow(); return p.run(); }";
    let files = [("api.hpp", header), ("main.cpp", caller)];
    let result = analyze_targets(&files, false);
    assert_eq!(result, analyze_targets(&files, true));
    for call in ["pointer:api::make", "copy:api::borrow"] {
        assert!(!result[call].as_ref().unwrap().has_body, "{call}");
    }
    for call in ["pointer:p->run", "copy:p.run"] {
        let target = result[call].as_ref().unwrap();
        assert_eq!(target.name, "api::Device::run");
        assert!(target.has_body);
    }
}

#[test]
fn named_aliases_only_block_their_own_type_lookup() {
    let header = r#"
namespace api {
using EventCallback = void (*)(int);
struct Base {};
struct Device : Base { int run() { return 1; } };
struct Owner : Base { using Callback = void (*)(int); Device* device; int apply(); };
}
namespace Alias { struct Device { int run() { return 2; } }; }
namespace other { struct Device { int run() { return 3; } }; }
namespace hidden { namespace Alias = other; }
namespace shadow { namespace Alias { struct Unrelated {}; } }
"#;
    let caller = r#"#include "api.hpp"
int entry(api::Device* d) { return d->run(); }
int api::Owner::apply() { return device->run(); }
namespace hidden { int entry(Alias::Device* d) { return d->run(); } }
namespace shadow { int entry(Alias::Device* d) { return d->run(); } }
"#;
    let files = [("api.hpp", header), ("main.cpp", caller)];
    let result = analyze(&files, false);
    assert_eq!(result, analyze(&files, true));
    assert_eq!(result["entry:d->run"].as_deref(), Some("api::Device::run"));
    assert_eq!(
        result["api::Owner::apply:device->run"].as_deref(),
        Some("api::Device::run")
    );
    assert_eq!(result["hidden::entry:d->run"], None);
    assert_eq!(result["shadow::entry:d->run"], None);
}

#[test]
fn field_type_lookup_uses_its_declaration_context_not_later_caller_imports() {
    let header = "namespace api { struct Device { int run() { return 1; } }; struct Owner { Device* device; int apply(); }; }";
    let noise = "namespace imported { struct Device { int run() { return 2; } }; } namespace api { using namespace imported; }";
    let source = "#include \"api.hpp\"\n#include \"noise.hpp\"\nint api::Owner::apply() { return device->run(); }";
    let files = [
        ("api.hpp", header),
        ("noise.hpp", noise),
        ("main.cpp", source),
    ];
    let result = analyze(&files, false);
    assert_eq!(result, analyze(&files, true));
    assert_eq!(
        result["api::Owner::apply:device->run"].as_deref(),
        Some("api::Device::run")
    );
}

#[test]
fn recovered_class_body_does_not_resolve_as_namespace_functions() {
    let source = r#"
namespace api {
struct Value {};
Value& real() { static Value v; return v; }
Value& outside() { return real(); }
class UNKNOWN_EXPORT Device : public support::Base<Device> {
public:
    Value& get() { return real(); }
    Value& apply() { return get(); }
};
}
"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
    assert_eq!(result.get("api::apply:get"), Some(&None), "{result:#?}");
    for (call, target) in &result {
        if call != "api::outside:real" {
            assert_eq!(target, &None, "{call}: {result:#?}");
        }
    }
    // An unrelated recovery error must not disable clean neighboring scopes.
    assert_eq!(
        result.get("api::outside:real").and_then(|v| v.as_deref()),
        Some("api::real"),
        "{result:#?}"
    );
}

#[test]
fn namespace_recovery_cannot_supply_member_qualifiers_or_this() {
    let source = r#"
namespace api {
int real() { return 1; }
int misplaced() const { return real(); }
void pseudo() { this->real(); }
void entry() { misplaced(); real(); }
struct Device {
    int ordinary() const { return api::real(); }
    int qualified() const;
};
int Device::qualified() const { return api::real(); }
}
"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
    for call in [
        "api::misplaced:real",
        "api::pseudo:this->real",
        "api::entry:misplaced",
    ] {
        assert_eq!(result.get(call), Some(&None), "{call}: {result:#?}");
    }
    for call in [
        "api::entry:real",
        "api::Device::ordinary:api::real",
        "api::Device::qualified:api::real",
    ] {
        assert_eq!(
            result.get(call).and_then(|target| target.as_deref()),
            Some("api::real"),
            "{call}: {result:#?}"
        );
    }
}

#[test]
fn explicit_this_capture_preserves_enclosing_object_boundaries() {
    let source = r#"
namespace api {
struct Device {
    void run() {}
    void captured() { auto task = [this] { this->run(); run(); }; }
    void nested() { auto task = [this] { auto inner = [this] { this->run(); run(); }; }; }
    void missing() { auto task = [] { this->run(); run(); }; }
    void broken_outer() { auto task = [] { auto inner = [this] { this->run(); run(); }; }; }
    void copied() { auto task = [*this] { this->run(); run(); }; }
    void other(Device* device) { auto task = [device] { device->run(); }; }
};
}

"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
    for name in ["captured", "nested"] {
        let bare = lambda_call_key(
            source,
            &format!("void {name}()"),
            "run();",
            "api::Device::",
            "run",
        );
        assert_eq!(
            result[&bare].as_deref(),
            Some("api::Device::run"),
            "{result:#?}"
        );
        let call = lambda_call_key(
            source,
            &format!("void {name}()"),
            "run();",
            "api::Device::",
            "this->run",
        );
        assert_eq!(
            result[&call].as_deref(),
            Some("api::Device::run"),
            "{result:#?}"
        );
    }
    for name in ["missing", "broken_outer", "copied"] {
        let bare = lambda_call_key(
            source,
            &format!("void {name}()"),
            "run();",
            "api::Device::",
            "run",
        );
        assert_eq!(result[&bare], None, "{result:#?}");
        let call = lambda_call_key(
            source,
            &format!("void {name}()"),
            "run();",
            "api::Device::",
            "this->run",
        );
        assert_eq!(result[&call], None, "{result:#?}");
    }
    assert_eq!(
        result[&lambda_call_key(
            source,
            "void other(",
            "device->run();",
            "api::Device::",
            "device->run"
        )],
        Some("api::Device::run".into())
    );
}

#[test]
fn hidden_friend_operator_calls_do_not_fall_back_to_unrelated_namespace_overloads() {
    let source = r#"
namespace api {
int selected(int value) { return value; }
struct Tag {
    friend int selected(Tag value) { return 2; }
    friend int operator<<(Tag tag, int value) { return selected(tag); }
};
int ordinary() { return selected(1); }
}
"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
    assert_eq!(result["api::Tag::operator<<:selected"], None, "{result:#?}");
    assert_eq!(
        result["api::ordinary:selected"].as_deref(),
        Some("api::selected")
    );
}

#[test]
fn nonvirtual_members_use_instantiated_base_name_absence_without_selecting_template_methods() {
    let header = r#"
namespace support {
struct Tag { virtual int run() { return 90; } };
template<class T> struct Empty { virtual int unrelated() { return 4; } };
template<class T> struct Layer : Empty<T>, T {};
}
"#;
    let source = r#"
#include "template.hpp"
namespace app { struct Tag {}; }
// specialization
namespace app {
struct Device : support::Layer<Tag> {
    int run() { return 7; }
    int implicit() { return run(); }
    int explicit_this() { return this->run(); }
};
int entry(Device* device) { return device->run(); }
}
"#;
    let keys = [
        "app::entry:device->run",
        "app::Device::implicit:run",
        "app::Device::explicit_this:this->run",
    ];
    let positive = analyze(&[("template.hpp", header), ("main.cpp", source)], false);
    assert_eq!(
        positive,
        analyze(&[("template.hpp", header), ("main.cpp", source)], true)
    );
    for key in keys {
        assert_eq!(
            positive[key].as_deref(),
            Some("app::Device::run"),
            "{positive:#?}"
        );
    }
    let virtual_tag = source.replace(
        "struct Tag {};",
        "struct Tag { virtual int run() { return 1; } };",
    );
    let hidden_virtual = source.replace(
        "struct Tag {};",
        "struct Top { virtual int run() { return 1; } }; struct Tag : Top { int run(int) { return 2; } };",
    );
    let specialization = source.replace(
        "// specialization",
        "template<> struct support::Empty<app::Tag> { virtual int run() { return 3; } };",
    );
    let missing_include = source.replace("#include \"template.hpp\"", "");
    let written_virtual =
        source.replace("int run() { return 7; }", "virtual int run() { return 7; }");
    for negative in [
        &virtual_tag,
        &hidden_virtual,
        &specialization,
        &missing_include,
        &written_virtual,
    ] {
        let result = analyze(&[("template.hpp", header), ("main.cpp", negative)], false);
        assert_eq!(
            result,
            analyze(&[("template.hpp", header), ("main.cpp", negative)], true)
        );
        for key in keys {
            assert_eq!(result[key], None, "{negative}\n{result:#?}");
        }
    }
    let missing_base = header.replace(
        "struct Empty { virtual int unrelated() { return 4; } };",
        "struct Empty;",
    );
    let result = analyze(
        &[("template.hpp", &missing_base), ("main.cpp", source)],
        false,
    );
    assert_eq!(
        result,
        analyze(
            &[("template.hpp", &missing_base), ("main.cpp", source)],
            true
        )
    );
    for key in keys {
        assert_eq!(result[key], None, "{result:#?}");
    }
}

#[test]
fn instantiated_base_names_do_not_hide_unrelated_outer_receiver_types() {
    let template = r#"
namespace support {
struct Device {}; // Never substitute the caller's Tag in this namespace.
struct Tag { using Handle = Device; };
template<class T> struct Empty {};
template<class T> struct Layer : Empty<T>, T {};
}
"#;
    let source = r#"
#include "template.hpp"
namespace app {
struct Device { int run() { return 1; } };
struct Tag {};
template<class T> struct Handle { T* operator->() const { return nullptr; } };
struct Driver : support::Layer<Tag> {
    Handle<Device> field;
    int invoke() { return field->run(); }
};
}
"#;
    let files = [("template.hpp", template), ("main.cpp", source)];
    let result = analyze(&files, false);
    assert_eq!(result, analyze(&files, true));
    assert_eq!(
        result["app::Driver::invoke:field->run"].as_deref(),
        Some("app::Device::run")
    );
    let hidden = source.replace("struct Tag {};", "struct Tag { using Handle = Device; };");
    let result = analyze(&[("template.hpp", template), ("main.cpp", &hidden)], false);
    assert_eq!(result["app::Driver::invoke:field->run"], None);
    let missing = template.replace(
        "template<class T> struct Empty {};",
        "template<class T> struct Empty;",
    );
    let result = analyze(&[("template.hpp", &missing), ("main.cpp", source)], false);
    assert_eq!(result["app::Driver::invoke:field->run"], None);
}

#[test]
fn template_base_specializations_cannot_be_replaced_by_the_primary_name_inventory() {
    let header = r#"
namespace support {
template<class T> struct Empty {};
template<class T> struct Layer : Empty<T> {};
}
"#;
    let source = r#"
#include "template.hpp"
struct Tag {};
struct Device { int run() { return 1; } };
template<> struct support::Empty<Tag> { using Device = int; };
struct Driver : support::Layer<Tag> {
    Device* field;
    int invoke() { return field->run(); }
};
"#;
    let result = analyze(&[("template.hpp", header), ("main.cpp", source)], false);
    assert_eq!(result["Driver::invoke:field->run"], None);
    assert_eq!(
        result,
        analyze(&[("template.hpp", header), ("main.cpp", source)], true)
    );
}

#[test]
fn an_injected_base_class_name_cannot_be_skipped_for_an_outer_same_name_type() {
    let source = r#"
namespace actual { struct Device { int run() { return 1; } }; }
struct Device { int run() { return 2; } };
struct Driver : actual::Device {
    Device* field;
    int invoke() { return field->run(); }
};
"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(
        result["Driver::invoke:field->run"],
        Some("actual::Device::run".into())
    );
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
}

#[test]
fn qualified_nested_definitions_supply_receivers_without_borrowing_other_bodies() {
    let header = "namespace api { struct Device { int read(); }; struct Outer { struct Inner; }; }";
    let body = "#include \"api.hpp\"\nint api::Device::read() { return 1; }";
    for definition in [
        "namespace api { struct Outer::Inner { Device* field; int invoke() { return field->read(); } }; }",
        "namespace api { struct Outer /* scope */ :: Inner { Device* field; int invoke() { return field->read(); } }; }",
        "struct api::Outer::Inner { Device* field; int invoke() { return field->read(); } };",
        "struct ::api::Outer::Inner { Device* field; int invoke() { return field->read(); } };",
    ] {
        let source = format!("#include \"api.hpp\"\n{definition}");
        let files = [
            ("api.hpp", header),
            ("body.cpp", body),
            ("main.cpp", source.as_str()),
        ];
        let result = analyze_targets(&files, false);
        assert_eq!(result, analyze_targets(&files, true));
        assert_eq!(
            result["api::Outer::Inner::invoke:field->read"],
            Some(Target {
                name: "api::Device::read".into(),
                path: "body.cpp".into(),
                has_body: true
            })
        );
        let virtual_header = header.replace("int read();", "virtual int read();");
        assert_eq!(
            analyze(
                &[
                    ("api.hpp", &virtual_header),
                    ("body.cpp", body),
                    ("main.cpp", &source)
                ],
                false
            )["api::Outer::Inner::invoke:field->read"],
            None
        );
        let hidden = source.replace("Device* field;", "using Device = int; Device* field;");
        assert_eq!(
            analyze(
                &[
                    ("api.hpp", header),
                    ("body.cpp", body),
                    ("main.cpp", &hidden)
                ],
                false
            )["api::Outer::Inner::invoke:field->read"],
            None
        );
    }
    // A definition outside the include inputs must not complete a declared receiver.
    let source =
        "#include \"api.hpp\"\nint entry(api::Outer::Inner* inner) { return inner->read(); }";
    let isolated = "#include \"api.hpp\"\nstruct api::Outer::Inner { int read() { return 1; } };";
    assert_eq!(
        analyze(
            &[
                ("api.hpp", header),
                ("main.cpp", source),
                ("isolated.cpp", isolated)
            ],
            false
        )["entry:inner->read"],
        None
    );
    let incomplete = "struct Device { int read() { return 1; } }; struct Missing::Inner { Device* field; int invoke() { return field->read(); } };";
    assert_eq!(
        analyze(&[("main.cpp", incomplete)], false)["Missing::Inner::invoke:field->read"],
        None
    );
    // An explicitly global type does not need unknown enclosing-name lookup.
    let explicit = incomplete.replace("Device* field", "::Device* field");
    assert_eq!(
        analyze(&[("main.cpp", &explicit)], false)["Missing::Inner::invoke:field->read"],
        Some("Device::read".into())
    );
}

#[test]
fn nested_bases_use_their_declaration_scope_and_inherited_class_names() {
    for parent in [
        "struct Nested : virtual Root {}; struct Device : Nested",
        "struct Holder : Root { struct Nested : virtual Root {}; }; struct Device : Holder::Nested",
        "struct Holder : Root { struct Nested : virtual ::api::Root {}; }; struct Device : Holder::Nested",
    ] {
        let source = format!(
            "namespace api {{ struct Root {{}}; {parent} {{ int read() {{ return 7; }} }}; }} int entry(api::Device* device) {{ return device->read(); }}"
        );
        let result = analyze(&[("main.cpp", &source)], false);
        assert_eq!(
            result["entry:device->read"],
            Some("api::Device::read".into()),
            "{source}"
        );
        assert_eq!(result, analyze(&[("main.cpp", &source)], true));
    }
}

#[test]
fn inherited_nested_types_respect_own_declarations_aliases_and_type_ambiguity() {
    let source = r#"
struct Token { int run() { return 99; } };
struct Parent { struct Token { int run() { return 1; } }; };
struct Holder : Parent {
    Token* field;
    int invoke() { return field->run(); }
};
"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(
        result["Holder::invoke:field->run"],
        Some("Parent::Token::run".into())
    );
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
    let own = source.replace(
        "Token* field;",
        "struct Token { int run() { return 2; } }; Token* field;",
    );
    assert_eq!(
        analyze(&[("main.cpp", &own)], false)["Holder::invoke:field->run"],
        Some("Holder::Token::run".into())
    );
    let alias = source.replace("Token* field;", "using Token = ::Token; Token* field;");
    assert_eq!(
        analyze(&[("main.cpp", &alias)], false)["Holder::invoke:field->run"],
        Some("Token::run".into())
    );
    let ambiguous = source.replace(
        "struct Holder : Parent",
        "struct Other { struct Token { int run() { return 3; } }; }; struct Holder : Parent, Other",
    );
    assert_eq!(
        analyze(&[("main.cpp", &ambiguous)], false)["Holder::invoke:field->run"],
        None
    );
    let shared = source.replace(
        "struct Holder : Parent",
        "struct Left : Parent {}; struct Right : Parent {}; struct Holder : Left, Right",
    );
    assert_eq!(
        analyze(&[("main.cpp", &shared)], false)["Holder::invoke:field->run"],
        Some("Parent::Token::run".into())
    );
    let enclosing = source
        .replace(
            "struct Parent { struct Token { int run() { return 1; } }; };",
            "namespace other { struct Token { int run() { return 3; } }; struct Parent {}; }",
        )
        .replace("struct Holder : Parent", "struct Holder : other::Parent");
    let result = analyze(&[("main.cpp", &enclosing)], false);
    // A base's enclosing namespace is not part of inherited member lookup.
    assert_eq!(
        result["Holder::invoke:field->run"],
        Some("Token::run".into())
    );
    assert_eq!(result, analyze(&[("main.cpp", &enclosing)], true));
}

#[test]
fn nested_base_lookup_preserves_inherited_virtuals_and_missing_input() {
    let source = r#"
namespace api {
struct Root {};
struct Holder : Root { struct Nested : virtual Root {}; };
struct Device : Holder::Nested { int read() { return 7; } };
}
int entry(api::Device* device) { return device->read(); }
"#;
    for (before, after) in [
        ("struct Root {};", "struct Root { virtual int read(); };"),
        ("struct Root {};", "struct Root;"),
        (
            "struct Nested : virtual Root",
            "using Root = ::Unknown; struct Nested : virtual Root",
        ),
    ] {
        let unknown = source.replace(before, after);
        let result = analyze(&[("main.cpp", &unknown)], false);
        assert_eq!(result["entry:device->read"], None, "{unknown}");
        assert_eq!(result, analyze(&[("main.cpp", &unknown)], true));
    }
    // An indexed but unincluded same-name declaration is not an input repair.
    let source = source.replace("struct Root {};", "struct Root;");
    let result = analyze(
        &[
            ("main.cpp", &source),
            ("unrelated.hpp", "namespace api { struct Root {}; }"),
        ],
        false,
    );
    assert_eq!(result["entry:device->read"], None);
}

#[test]
fn closure_receivers_use_local_capture_facts_before_enclosing_fields() {
    let source = r#"
namespace receiver_fixture {
struct Worker { void run() {} };
struct Other { void run() {} };
struct Owner {
    Worker* worker;
    void field() { auto task = [this] { worker->run(); }; }
    void written_field() { auto task = [this] { this->worker->run(); }; }
    void nested() { auto task = [this] { auto inner = [this] { worker->run(); }; }; }
    void shadow(Other* worker) { auto task = [worker] { worker->run(); }; }
    void initialized_shadow(Other* other) { auto task = [this, worker=other] { worker->run(); }; }
    void missing() { auto task = [] { worker->run(); }; }
    void broken_outer() { auto task = [] { auto inner = [this] { worker->run(); }; }; }
    void copied() { auto task = [*this] { worker->run(); }; }
    void default_capture() { auto task = [=] { worker->run(); }; }
};
void local() { auto task = [] { Worker worker; worker.run(); }; }
void reference(Worker& worker) { auto task = [&worker] { worker.run(); }; }
void mutable_copy(Worker worker) { auto task = [worker]() mutable { worker.run(); }; }
void const_copy(Worker worker) { auto task = [worker] { worker.run(); }; }
void pointer(Worker* worker) { auto task = [worker] { worker->run(); }; }
void absent(Worker* worker) { auto task = [] { worker->run(); }; }
void const_pointee(const Worker* worker) { auto task = [worker] { worker->run(); }; }
}
"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
    for (function, expression, owner, target) in [
        ("field", "worker->run", "Owner::", Some("Worker::run")),
        (
            "written_field",
            "this->worker->run",
            "Owner::",
            Some("Worker::run"),
        ),
        ("nested", "worker->run", "Owner::", Some("Worker::run")),
        ("shadow", "worker->run", "Owner::", Some("Other::run")),
        ("initialized_shadow", "worker->run", "Owner::", None),
        ("missing", "worker->run", "Owner::", None),
        ("broken_outer", "worker->run", "Owner::", None),
        ("copied", "worker->run", "Owner::", None),
        ("default_capture", "worker->run", "Owner::", None),
        ("local", "worker.run", "", Some("Worker::run")),
        ("reference", "worker.run", "", Some("Worker::run")),
        ("mutable_copy", "worker.run", "", Some("Worker::run")),
        ("const_copy", "worker.run", "", None),
        ("pointer", "worker->run", "", Some("Worker::run")),
        ("absent", "worker->run", "", None),
        ("const_pointee", "worker->run", "", None),
    ] {
        let key = lambda_call_key(
            source,
            &format!("void {function}("),
            expression,
            &format!("receiver_fixture::{owner}"),
            expression,
        );
        assert_eq!(
            result[&key],
            target.map(|name| format!("receiver_fixture::{name}")),
            "{key}"
        );
    }
}

#[test]
fn captured_field_access_does_not_discard_enclosing_object_constraints() {
    let source = r#"
struct Worker { void run() {} };
struct Owner {
    Worker worker;
    void ordinary() { auto task = [this] { worker.run(); }; }
    void constant() const { auto task = [this] { worker.run(); }; }
    void nested_constant() const { auto outer = [this] { auto inner = [this] { worker.run(); }; }; }
    void changing() volatile { auto task = [this] { worker.run(); }; }
    static void no_object() { auto task = [this] { worker.run(); }; }
};
"#;
    let result = analyze(&[("main.cpp", source)], false);
    assert_eq!(result, analyze(&[("main.cpp", source)], true));
    for name in [
        "ordinary",
        "constant",
        "nested_constant",
        "changing",
        "no_object",
    ] {
        let key = lambda_call_key(
            source,
            &format!("void {name}("),
            "worker.run",
            "Owner::",
            "worker.run",
        );
        assert_eq!(
            result[&key].as_deref(),
            (name == "ordinary").then_some("Worker::run"),
            "{key}"
        );
    }
}

#[test]
fn explicit_template_reference_calls_select_declarations_and_matching_bodies() {
    let files = [
        (
            "reader.h",
            "struct Reader { template<class T> bool read(T& value); template<class T> T read(); bool run(int& value); };",
        ),
        (
            "main.cpp",
            "#include \"reader.h\"\ntemplate<class U> bool Reader::read(U& value) { return true; } template<class U> U Reader::read() { return U{}; } bool Reader::run(int& value) { return read<int>(value); }",
        ),
    ];
    for scoped in [false, true] {
        let calls = analyze_targets(&files, scoped);
        assert_eq!(
            calls["Reader::run:read<int>"],
            Some(Target {
                name: "Reader::read".into(),
                path: "main.cpp".into(),
                has_body: true
            }),
            "{calls:#?}"
        );
    }
}

#[test]
fn explicit_template_calls_keep_argument_conflicts_and_competing_templates_unknown() {
    let source = r#"
template<class T> bool read(T& value) { return true; }
void exact(int& value) { read<int>(value); }
void incompatible(long& value) { read<int>(value); }
void readonly(const int& value) { read<int>(value); }
void temporary() { read<int>(1); }
template<class T> bool view(const T& value) { return true; }
void cv(int& value) { view<const int>(value); }
namespace special {
template<class T> bool read(T& value) { return true; }
template<> bool read<int>(int& value) { return false; }
void invoke(int& value) { read<int>(value); }
}
namespace competing {
template<class T> bool read(T& value) { return true; }
template<class T> bool read(const T& value) { return false; }
void invoke(int& value) { read<int>(value); }
}
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("main.cpp", source)], scoped);
        assert_eq!(calls["exact:read<int>"].as_deref(), Some("read"));
        assert_eq!(calls["cv:view<const int>"].as_deref(), Some("view"));
        for caller in [
            "incompatible",
            "readonly",
            "temporary",
            "special::invoke",
            "competing::invoke",
        ] {
            assert_eq!(
                calls[&format!("{caller}:read<int>")],
                None,
                "{caller}: {calls:#?}"
            );
        }
    }
}

#[test]
fn explicit_template_member_calls_require_the_receiver_and_preserve_static_rules() {
    let source = r#"
struct Reader {
    template<class T> bool read(T& value) { return true; }
    template<class T> static bool decode(T& value) { return true; }
    static bool invalid(int& value) { return read<int>(value); }
    static bool external_invalid(int& value);
};
bool Reader::external_invalid(int& value) { return read<int>(value); }
void member(Reader& reader, int& value) { reader.read<int>(value); }
void static_member(int& value) { Reader::decode<int>(value); }
void missing_object(int& value) { Reader::read<int>(value); }
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("main.cpp", source)], scoped);
        assert_eq!(
            calls["member:reader.read<int>"].as_deref(),
            Some("Reader::read"),
            "{calls:#?}"
        );
        assert_eq!(
            calls["static_member:Reader::decode<int>"].as_deref(),
            Some("Reader::decode"),
            "{calls:#?}"
        );
        for call in [
            "Reader::invalid:read<int>",
            "Reader::external_invalid:read<int>",
            "missing_object:Reader::read<int>",
        ] {
            assert_eq!(calls[call], None, "{call}: {calls:#?}");
        }
    }
}

#[test]
fn explicit_template_substitution_checks_unsupplied_parameters_and_return_shapes() {
    let source = r#"
template<class T> T result();
template<class T> T& reference_result();
template<class T> T* pointer_result();
template<class T> void defaulted(T value = 0);
void valid() { result<void>(); reference_result<int>(); pointer_result<void>(); defaulted<int>(); }
void invalid() { reference_result<void>(); defaulted<void>(); }
"#;
    for scoped in [false, true] {
        let calls = analyze(&[("main.cpp", source)], scoped);
        for (call, target) in [
            ("result<void>", "result"),
            ("reference_result<int>", "reference_result"),
            ("pointer_result<void>", "pointer_result"),
            ("defaulted<int>", "defaulted"),
        ] {
            assert_eq!(
                calls[&format!("valid:{call}")].as_deref(),
                Some(target),
                "{calls:#?}"
            );
        }
        assert_eq!(calls["invalid:reference_result<void>"], None);
        assert_eq!(calls["invalid:defaulted<void>"], None);
    }
}

#[test]
fn receiver_completion_uses_the_declaration_identity_at_member_use() {
    let definition =
        "#include \"api.hpp\"\nnamespace lib { struct Device { int read() { return 1; } }; }";
    for written in ["lib::Device", "Alias"] {
        let header = format!(
            "#pragma once\nnamespace lib {{ struct Device; }}\nnamespace app {{ using Alias = lib::Device; struct Holder {{ {written}* field; int invoke(); }}; }}"
        );
        for shadow in [
            "",
            "namespace app { namespace lib { struct Device { int read() { return 99; } }; } }",
        ] {
            let source = format!(
                "#include \"api.hpp\"\n#include \"definition.hpp\"\n{shadow}\nint app::Holder::invoke() {{ return field->read(); }}"
            );
            for scoped in [false, true] {
                let result = analyze_targets(
                    &[
                        ("api.hpp", &header),
                        ("definition.hpp", definition),
                        ("main.cpp", &source),
                    ],
                    scoped,
                );
                assert_eq!(
                    result["app::Holder::invoke:field->read"],
                    Some(Target {
                        name: "lib::Device::read".into(),
                        path: "definition.hpp".into(),
                        has_body: true
                    }),
                    "written={written}, shadow={shadow}, scoped={scoped}"
                );
            }
        }
    }
}

#[test]
fn receiver_completion_keeps_unseen_late_and_conflicting_definitions_unknown() {
    let header = "#pragma once\nnamespace lib { struct Device; } namespace app { struct Holder { lib::Device* field; int invoke(); }; }";
    let definition =
        "#include \"api.hpp\"\nnamespace lib { struct Device { int read() { return 1; } }; }";
    let invoke = "int app::Holder::invoke() { return field->read(); }";
    let cases = [
        format!("#include \"api.hpp\"\n{invoke}"),
        format!("#include \"api.hpp\"\n{invoke}\n#include \"definition.hpp\""),
        format!(
            "#include \"api.hpp\"\n{invoke}\nnamespace lib {{ struct Device {{ int read() {{ return 2; }} }}; }}"
        ),
        format!(
            "#include \"api.hpp\"\n#include \"definition.hpp\"\n#include \"conflict.hpp\"\n{invoke}"
        ),
    ];
    for source in cases {
        for scoped in [false, true] {
            let result = analyze(
                &[
                    ("api.hpp", header),
                    ("definition.hpp", definition),
                    (
                        "conflict.hpp",
                        "namespace lib { struct Device { int read() { return 3; } }; }",
                    ),
                    ("main.cpp", &source),
                ],
                scoped,
            );
            assert_eq!(result["app::Holder::invoke:field->read"], None, "{source}");
        }
    }
    // A nearer incomplete declaration hides the global complete type.
    let hidden = "struct Device { int read() { return 9; } }; namespace app { struct Device; struct Holder { Device* field; int invoke(); }; } int app::Holder::invoke() { return field->read(); }";
    for scoped in [false, true] {
        assert_eq!(
            analyze(&[("main.cpp", hidden)], scoped)["app::Holder::invoke:field->read"],
            None
        );
    }
}

#[test]
fn operator_pointee_completion_preserves_the_written_type_scope() {
    for pointer in [
        "struct Pointer { lib::Device* operator->(); };",
        "template<class T> struct Pointer { T* operator->(); };",
    ] {
        let instance = if pointer.starts_with("template") {
            "Pointer<lib::Device>"
        } else {
            "Pointer"
        };
        let header = format!(
            "#pragma once\nnamespace lib {{ struct Device; }} {pointer} namespace app {{ struct Holder {{ {instance} field; int invoke(); }}; }}"
        );
        let definition =
            "#include \"api.hpp\"\nnamespace lib { struct Device { int read() { return 1; } }; }";
        let source = "#include \"api.hpp\"\n#include \"definition.hpp\"\nnamespace app { namespace lib { struct Device { int read() { return 99; } }; } } int app::Holder::invoke() { return field->read(); }";
        for scoped in [false, true] {
            assert_eq!(
                analyze(
                    &[
                        ("api.hpp", &header),
                        ("definition.hpp", definition),
                        ("main.cpp", source)
                    ],
                    scoped
                )["app::Holder::invoke:field->read"],
                Some("lib::Device::read".into()),
                "{pointer}"
            );
        }
    }
}
