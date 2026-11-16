use std::sync::Arc;

use db::Store;
use extraction::ExtractionMode;
use filesync::{IncrementalPipeline, IndexPipeline, IndexPipelineOptions, NoopSink};
use types::{EdgeKind, ReferenceKind};

#[test]
fn stored_callers_keep_distinct_bodies_across_reindexing() {
    let project = tempfile::tempdir().unwrap();
    let source = "void first() {} void second() {}\n#if ENABLED\nvoid run() { first(); }\n#else\nvoid run() { second(); }\n#endif\nvoid entry() { run(); }\nvoid known(); void known() {} void entry_known() { known(); }\n";
    let path = project.path().join("main.cpp");
    std::fs::write(&path, source).unwrap();
    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();
    IndexPipeline::new(
        store.clone(),
        project.path().into(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .run(&NoopSink, &mut || false)
    .unwrap();
    for updated in [
        None,
        Some(format!("// relocated source declarations\n{source}")),
    ] {
        if let Some(updated) = updated {
            std::fs::write(&path, updated).unwrap();
            IncrementalPipeline::new(
                store.clone(),
                project.path().into(),
                ExtractionMode::Structural,
            )
            .sync(&NoopSink, &mut || false)
            .unwrap();
        }
        let runs = store.find_symbols_by_qname("run").unwrap();
        assert_eq!(
            runs.len(),
            2,
            "conditional bodies remain independently addressable"
        );
        let calls = store.get_all_call_references().unwrap();
        for name in ["first", "second"] {
            let call = calls
                .iter()
                .find(|r| r.kind == ReferenceKind::Call && r.name == name)
                .unwrap();
            let caller = store
                .find_symbol_by_id(&call.source_symbol.unwrap())
                .unwrap()
                .unwrap();
            assert!(
                caller.range.start_byte <= call.range.start_byte
                    && call.range.end_byte <= caller.range.end_byte
            );
            let target = store
                .find_symbol_by_id(
                    &call
                        .resolved
                        .as_ref()
                        .expect("independent target remains useful")
                        .symbol_id,
                )
                .unwrap()
                .unwrap();
            assert_eq!(target.name, name);
            assert!(
                store
                    .find_edges_by_source(&caller.id)
                    .unwrap()
                    .iter()
                    .any(|e| e.kind == EdgeKind::Calls
                        && e.target == target.id
                        && e.ref_id == Some(call.id))
            );
        }
        let call = calls.iter().find(|r| r.name == "run").unwrap();
        assert!(
            call.resolved.is_none(),
            "source occurrence identity does not choose an active conditional branch"
        );
        assert!(
            store
                .find_edges_by_source(&call.source_symbol.unwrap())
                .unwrap()
                .iter()
                .all(|e| e.kind != EdgeKind::Calls)
        );
        let call = calls.iter().find(|r| r.name == "known").unwrap();
        let target = store
            .find_symbol_by_id(
                &call
                    .resolved
                    .as_ref()
                    .expect("prototype and sole definition still associate")
                    .symbol_id,
            )
            .unwrap()
            .unwrap();
        assert_eq!(target.name, "known");
        assert_ne!(target.range, target.name_range);
    }
}
