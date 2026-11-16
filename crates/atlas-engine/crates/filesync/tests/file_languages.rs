use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use db::Store;
use extraction::ExtractionMode;
use filesync::{IncrementalPipeline, IndexPipeline, IndexPipelineOptions, NoopSink};
use types::Language;

#[test]
fn explicit_languages_discover_nonstandard_paths_and_preserve_them_during_sync() {
    let project = tempfile::tempdir().unwrap();
    let header = "int answer() { return 1; }";
    for path in ["api", "api.custom", "unconfigured"] {
        std::fs::write(project.path().join(path), header).unwrap();
    }
    std::fs::write(
        project.path().join("main.cpp"),
        "#include <api>\nint caller() { return answer(); }",
    )
    .unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = |languages| {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural)
                .with_file_languages(languages)
                .with_include_paths(vec![".".into()]),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    assert_eq!(run(Default::default()).discovered, 1);
    let languages = BTreeMap::from([
        ("api".into(), Language::Cpp),
        ("api.custom".into(), Language::Cpp),
    ]);
    let configured = run(languages.clone());
    assert_eq!(configured.discovered, 3);
    assert_eq!(configured.indexed, 2);
    let assert_call = || {
        let caller = store
            .find_symbols_by_qname("caller")
            .unwrap()
            .pop()
            .unwrap();
        let targets: Vec<_> = store
            .find_edges_by_source(&caller.id)
            .unwrap()
            .into_iter()
            .filter(|edge| edge.kind == types::EdgeKind::Calls)
            .collect();
        assert_eq!(targets.len(), 1);
        let expected = store
            .find_symbols_by_qname("answer")
            .unwrap()
            .into_iter()
            .find(|symbol| symbol.file_id == types::FileId::generate("api"))
            .unwrap();
        assert_eq!(
            targets[0].target, expected.id,
            "unrelated same-name header must not substitute"
        );
    };
    assert_call();
    assert_eq!(run(languages).indexed, 0);
    let sync = || {
        IncrementalPipeline::new(
            store.clone(),
            project.path().into(),
            ExtractionMode::Structural,
        )
        .sync(&NoopSink, &mut || false)
        .unwrap()
    };
    assert_eq!(sync().files_changed, 0);
    std::fs::write(
        project.path().join("api"),
        header.replace("return 1", "return 2"),
    )
    .unwrap();
    let changed = sync();
    assert_eq!(changed.files_reindexed, 1);
    assert_eq!(changed.files_removed, 0);
    assert_call();
    std::fs::remove_file(project.path().join("api.custom")).unwrap();
    assert_eq!(sync().files_removed, 1);
    assert_call();
    assert_eq!(run(Default::default()).discovered, 1);
    assert_eq!(store.list_files().unwrap().len(), 1);
    assert!(store.find_symbols_by_qname("answer").unwrap().is_empty());
}

#[test]
fn explicit_languages_do_not_bypass_scope_or_ignore_rules() {
    let project = tempfile::tempdir().unwrap();
    for path in ["allowed", "excluded", "ignored", "outside"] {
        std::fs::write(project.path().join(path), "int value;").unwrap();
    }
    std::fs::write(project.path().join(".atlasignore"), "ignored\n").unwrap();
    for path in [
        "excluded",
        "ignored",
        "outside",
        "missing",
        "../allowed",
        "/allowed",
    ] {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let result = IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural)
                .with_include_patterns(vec!["allowed".into(), "excluded".into(), "ignored".into()])
                .with_exclude_patterns(vec!["excluded".into()])
                .with_file_languages(BTreeMap::from([(path.into(), Language::Cpp)])),
        )
        .run(&NoopSink, &mut || false);
        assert!(result.is_err(), "{path}");
        assert!(store.list_files().unwrap().is_empty(), "{path}");
    }
}

#[test]
fn explicit_file_language_changes_reextract_unchanged_headers_and_survive_incremental_sync() {
    let project = tempfile::tempdir().unwrap();
    let header = "namespace api { class Reader { public: int read() { return 1; } }; }";
    std::fs::write(project.path().join("api.h"), header).unwrap();
    std::fs::write(
        project.path().join("plain.h"),
        "struct Plain { int value; };",
    )
    .unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    let run = |languages: BTreeMap<PathBuf, Language>| {
        IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural).with_file_languages(languages),
        )
        .run(&NoopSink, &mut || false)
        .unwrap()
    };
    run(Default::default());
    let file_language = |path: &str| {
        store
            .list_files()
            .unwrap()
            .into_iter()
            .find(|file| file.path == path)
            .unwrap()
            .language
    };
    assert_eq!(file_language("api.h"), Language::C);
    let languages = BTreeMap::from([(PathBuf::from("api.h"), Language::Cpp)]);
    let changed = run(languages.clone());
    assert_eq!(changed.indexed, 1);
    assert_eq!(file_language("api.h"), Language::Cpp);
    assert_eq!(file_language("plain.h"), Language::C);
    assert_eq!(
        store
            .find_symbols_by_qname("api::Reader::read")
            .unwrap()
            .len(),
        1
    );
    assert_eq!(run(languages).indexed, 0);

    std::fs::write(
        project.path().join("api.h"),
        header.replace("return 1", "return 2"),
    )
    .unwrap();
    IncrementalPipeline::new(
        store.clone(),
        project.path().into(),
        ExtractionMode::Structural,
    )
    .sync(&NoopSink, &mut || false)
    .unwrap();
    assert_eq!(file_language("api.h"), Language::Cpp);
    assert_eq!(
        store
            .find_symbols_by_qname("api::Reader::read")
            .unwrap()
            .len(),
        1
    );

    assert_eq!(run(Default::default()).indexed, 1);
    assert_eq!(file_language("api.h"), Language::C);
}

#[test]
fn file_language_overrides_cannot_expand_discovered_scope() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("kept.h"), "int value;").unwrap();
    std::fs::write(project.path().join("other.h"), "int other;").unwrap();
    for path in ["other.h", "missing.h", "../kept.h", "/kept.h"] {
        let store = Arc::new(Store::open_in_memory().unwrap());
        store.init_schema().unwrap();
        let result = IndexPipeline::new(
            store.clone(),
            project.path().into(),
            IndexPipelineOptions::new(ExtractionMode::Structural)
                .with_include_patterns(vec!["kept.h".into()])
                .with_file_languages(BTreeMap::from([(path.into(), Language::Cpp)])),
        )
        .run(&NoopSink, &mut || false);
        assert!(result.is_err(), "{path}");
        assert!(store.list_files().unwrap().is_empty(), "{path}");
    }
}

#[test]
fn language_change_resumes_resolution_after_canceled_extraction() {
    use filesync::{PhaseName, ProgressEvent, ProgressSink};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct CancelAfterWrite(AtomicBool);
    impl ProgressSink for CancelAfterWrite {
        fn emit(&self, event: ProgressEvent) {
            if matches!(
                event,
                ProgressEvent::PhaseFinished {
                    phase: PhaseName::DbWrite,
                    ..
                }
            ) {
                self.0.store(true, Ordering::Relaxed);
            }
        }
    }
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("api.h"),
        "int target() { return 1; } int caller() { return target(); }",
    )
    .unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    IndexPipeline::new(
        store.clone(),
        project.path().into(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .run(&NoopSink, &mut || false)
    .unwrap();
    let options = IndexPipelineOptions::new(ExtractionMode::Structural)
        .with_file_languages(BTreeMap::from([("api.h".into(), Language::Cpp)]));
    let canceled = CancelAfterWrite(AtomicBool::new(false));
    IndexPipeline::new(store.clone(), project.path().into(), options.clone())
        .run(&canceled, &mut || canceled.0.load(Ordering::Relaxed))
        .unwrap();
    assert!(canceled.0.load(Ordering::Relaxed));
    assert!(
        store
            .get_all_edges()
            .unwrap()
            .iter()
            .all(|edge| edge.kind != types::EdgeKind::Calls)
    );
    let resumed = IndexPipeline::new(store.clone(), project.path().into(), options)
        .run(&NoopSink, &mut || false)
        .unwrap();
    assert_eq!(resumed.indexed, 0, "extraction had already completed");
    let caller = store
        .find_symbols_by_qname("caller")
        .unwrap()
        .pop()
        .unwrap();
    let target = store
        .find_symbols_by_qname("target")
        .unwrap()
        .pop()
        .unwrap();
    assert!(
        store
            .find_edges_by_source(&caller.id)
            .unwrap()
            .iter()
            .any(|edge| edge.kind == types::EdgeKind::Calls && edge.target == target.id)
    );
}
