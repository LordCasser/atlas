use std::{path::PathBuf, sync::Arc};

use db::Store;
use extraction::ExtractionMode;
use filesync::{IndexPipeline, IndexPipelineOptions, NoopSink};
use types::EdgeKind;

fn call_files(store: &Store, caller: &str) -> Vec<String> {
    let caller = store.find_symbols_by_qname(caller).unwrap();
    assert_eq!(caller.len(), 1);
    let mut files = store
        .find_edges_by_source(&caller[0].id)
        .unwrap()
        .into_iter()
        .filter(|edge| edge.kind == EdgeKind::Calls)
        .map(|edge| {
            let symbol = store
                .get_all_symbols()
                .unwrap()
                .into_iter()
                .find(|symbol| symbol.id == edge.target)
                .unwrap();
            store.get_file(&symbol.file_id).unwrap().unwrap().path
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

#[test]
fn includes_use_exact_paths_order_and_scope_and_rebuild_unchanged_calls() {
    let project = tempfile::tempdir().unwrap();
    for (path, source) in [
        (
            "src/main.cpp",
            "#include <api.hpp>\n#include \"local.hpp\"\n#include <only-local.hpp>\n#include <ghost.hpp>\n#include <hidden.hpp>\nint pick() { return api::choose(); }\nint local_call() { return local(); }\nint local_angle() { return only_local(); }\nint missing() { return ghost(); }\nint outside() { return hidden(); }",
        ),
        ("src/local.hpp", "int local() { return 1; }"),
        ("src/only-local.hpp", "int only_local() { return 1; }"),
        (
            "first/api.hpp",
            "namespace api { int choose() { return 1; } }",
        ),
        (
            "second/api.hpp",
            "namespace api { int choose() { return 2; } }",
        ),
        ("first/local.hpp", "int local() { return 2; }"),
        ("first/ghost.hpp.extra.hpp", "int ghost() { return 1; }"),
        ("excluded/hidden.hpp", "int hidden() { return 1; }"),
        (
            "src/main.ts",
            "import { helper } from './dep'; export function tsCaller() { return helper(); }",
        ),
        ("src/dep.ts", "export function helper() { return 1; }"),
        (
            "first/unrelated.ts",
            "export function helper() { return 2; }",
        ),
    ] {
        let file = project.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, source).unwrap();
    }
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = |paths: &[&str]| {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural)
                .with_include_patterns(vec!["src/**".into(), "first/**".into(), "second/**".into()])
                .with_include_paths(paths.iter().map(PathBuf::from).collect()),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run(&["first", "second", "excluded"]);
    assert_eq!(call_files(&store, "pick"), ["first/api.hpp"]);
    assert_eq!(call_files(&store, "local_call"), ["src/local.hpp"]);
    for caller in ["local_angle", "missing", "outside"] {
        assert!(call_files(&store, caller).is_empty(), "{caller}");
    }
    assert_eq!(call_files(&store, "tsCaller"), ["src/dep.ts"]);
    assert_eq!(run(&["second", "first"]).indexed, 0);
    assert_eq!(call_files(&store, "pick"), ["second/api.hpp"]);
    assert_eq!(call_files(&store, "tsCaller"), ["src/dep.ts"]);
    assert_eq!(run(&[]).indexed, 0);
    assert!(call_files(&store, "pick").is_empty());
    assert_eq!(call_files(&store, "local_call"), ["src/local.hpp"]);
    assert_eq!(call_files(&store, "tsCaller"), ["src/dep.ts"]);
    assert_eq!(run(&["first"]).indexed, 0);
    assert_eq!(call_files(&store, "pick"), ["first/api.hpp"]);
}

#[test]
fn invalid_include_paths_do_not_clear_existing_facts() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("main.cpp"), "int kept() { return 1; }").unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    IndexPipeline::new(
        store.clone(),
        project.path().into(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .run(&NoopSink, &mut || false)
    .unwrap();
    std::fs::write(
        project.path().join("main.cpp"),
        "int replaced() { return 2; }",
    )
    .unwrap();
    for path in ["", "/tmp", "../outside", "first/../second"] {
        let result = IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural)
                .with_include_paths(vec![path.into()]),
        )
        .run(&NoopSink, &mut || false);
        assert!(result.is_err(), "{path}");
        assert_eq!(store.find_symbols_by_qname("kept").unwrap().len(), 1);
        assert!(store.find_symbols_by_qname("replaced").unwrap().is_empty());
    }
}
