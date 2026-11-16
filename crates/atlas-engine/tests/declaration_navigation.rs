#![cfg(feature = "cpp")]
use atlas_engine::{
    ExtractionMode, IndexPipeline, IndexPipelineOptions, NoopSink, Store,
    cpp_declaration_navigation,
};
use std::sync::Arc;

#[test]
fn declaration_navigation_keeps_base_identity_overloads_bodies_and_unknowns_separate() {
    let root = tempfile::tempdir().unwrap();
    let source = "namespace api { struct Base {}; using Alias = Base; struct Child : Alias { void run(int); void run(double); void missing(); void inline_run() {} }; void Child::run(int value) {} void Child::run(double value) {} struct Unknown : MissingBase {}; template<class T> struct Template {}; struct Specialized : Template<int> {}; } namespace noise { struct Base {}; struct Child : Base { void run(int) {} }; }";
    std::fs::write(root.path().join("main.cpp"), source).unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    IndexPipeline::new(
        store.clone(),
        root.path().into(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .run(&NoopSink, &mut || false)
    .unwrap();
    let before_calls = store.get_all_call_references().unwrap();
    let before_edges = store.get_all_edges().unwrap();
    let result = cpp_declaration_navigation(&store, &|| false).unwrap();
    let child = result
        .iter()
        .find(|r| r.declaration.qualified_name == "api::Child")
        .unwrap();
    assert_eq!(
        child.bases.as_ref().unwrap()[0]
            .target
            .as_ref()
            .unwrap()
            .qualified_name,
        "api::Base"
    );
    let runs: Vec<_> = child
        .members
        .iter()
        .filter(|m| m.declaration.name == "run")
        .collect();
    assert_eq!(runs.len(), 2);
    for member in runs {
        assert_eq!(member.definitions.len(), 1, "{member:?}");
        let body = &member.definitions[0];
        let written = &source[body.range.start_byte as usize..body.range.end_byte as usize];
        let parameter = &member.callable.as_ref().unwrap().parameter_types[0];
        assert!(
            written.contains(&format!("run({parameter} value)")),
            "{written} versus {parameter}"
        );
        assert_ne!(body.id, member.declaration.id);
    }
    assert!(
        child
            .members
            .iter()
            .find(|m| m.declaration.name == "missing")
            .unwrap()
            .definitions
            .is_empty()
    );
    let inline = child
        .members
        .iter()
        .find(|m| m.declaration.name == "inline_run")
        .unwrap();
    assert_eq!(inline.definitions[0].id, inline.declaration.id);
    let noise = result
        .iter()
        .find(|r| r.declaration.qualified_name == "noise::Child")
        .unwrap();
    assert_eq!(
        noise.bases.as_ref().unwrap()[0]
            .target
            .as_ref()
            .unwrap()
            .qualified_name,
        "noise::Base"
    );
    for name in ["api::Unknown", "api::Specialized"] {
        let base = &result
            .iter()
            .find(|r| r.declaration.qualified_name == name)
            .unwrap()
            .bases
            .as_ref()
            .unwrap()[0];
        assert!(base.target.is_none(), "{base:?}");
        assert!(base.written.range.start_byte < base.written.range.end_byte);
    }
    assert!(cpp_declaration_navigation(&store, &|| true).is_err());
    assert_eq!(before_calls, store.get_all_call_references().unwrap());
    assert_eq!(before_edges, store.get_all_edges().unwrap());
}
