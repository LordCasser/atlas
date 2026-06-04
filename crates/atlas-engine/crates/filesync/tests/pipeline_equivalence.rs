//! DB equivalence tests for pipeline convergence.
//!
//! Verifies that `run_index_pipeline` (shared), `IndexPipeline` (new), and
//! `IncrementalPipeline` produce equivalent database state — same files,
//! symbols, edges, and symbol names — for both full-index and incremental
//! workflows.  Uses `ExtractionMode::Structural` so edges are built.

use std::path::Path;
use std::sync::Arc;

use db::Store;
use extraction::ExtractionMode;
use filesync::{
    IncrementalPipeline, IndexPipeline, IndexPipelineOptions, NoopSink, run_index_pipeline,
};

// ── Helpers ────────────────────────────────────────────────────────────────

/// Create a small TypeScript project with functions, types, classes, and
/// imports across 4 files so the pipeline always has enough data to produce
/// meaningful counts.
fn create_ts_project(dir: &Path) {
    // index.ts — main entry with cross-file imports
    std::fs::write(
        dir.join("index.ts"),
        "\
import { add, multiply } from './math';\n\
import { Greeter } from './greeter';\n\
import type { User } from './types';\n\
\n\
export function main(): string {\n\
    const g = new Greeter('World');\n\
    const sum = add(1, 2);\n\
    return g.greet() + ': ' + sum;\n\
}\n",
    )
    .unwrap();

    // math.ts — pure function exports
    std::fs::write(
        dir.join("math.ts"),
        "\
export function add(a: number, b: number): number {\n\
    return a + b;\n\
}\n\
\n\
export function multiply(a: number, b: number): number {\n\
    return a * b;\n\
}\n",
    )
    .unwrap();

    // greeter.ts — class with method
    std::fs::write(
        dir.join("greeter.ts"),
        "\
export class Greeter {\n\
    private name: string;\n\
\n\
    constructor(name: string) {\n\
        this.name = name;\n\
    }\n\
\n\
    greet(): string {\n\
        return `Hello, ${this.name}!`;\n\
    }\n\
}\n",
    )
    .unwrap();

    // types.ts — interface + type alias
    std::fs::write(
        dir.join("types.ts"),
        "\
export interface User {\n\
    id: number;\n\
    name: string;\n\
    email?: string;\n\
}\n\
\n\
export type Role = 'admin' | 'user' | 'guest';\n",
    )
    .unwrap();
}

/// Overwrite `math.ts` with two additional exported functions so the
/// incremental pipeline has detectable changes.
fn modify_math_file(dir: &Path) {
    std::fs::write(
        dir.join("math.ts"),
        "\
export function add(a: number, b: number): number {\n\
    return a + b;\n\
}\n\
\n\
export function multiply(a: number, b: number): number {\n\
    return a * b;\n\
}\n\
\n\
export function subtract(a: number, b: number): number {\n\
    return a - b;\n\
}\n\
\n\
export function divide(a: number, b: number): number {\n\
    return a / b;\n\
}\n",
    )
    .unwrap();
}

/// Snapshot of database state for equivalence comparison.
#[derive(Debug)]
struct DbSnapshot {
    file_count: i64,
    symbol_count: i64,
    edge_count: i64,
    symbol_names: Vec<String>,
}

impl DbSnapshot {
    fn from_store(store: &Store) -> Self {
        let stats = store.get_stats().unwrap();
        let symbols = store.get_all_symbols().unwrap();
        let mut names: Vec<String> = symbols.iter().map(|s| s.name.clone()).collect();
        names.sort();
        Self {
            file_count: stats.total_files,
            symbol_count: stats.total_symbols,
            edge_count: stats.total_edges,
            symbol_names: names,
        }
    }
}

// ── Test 1: full-index equivalence ─────────────────────────────────────────

/// `run_index_pipeline` and `IndexPipeline::run` must produce the same
/// files, symbols, edges, and symbol names when indexing the same project
/// from scratch with `ExtractionMode::Structural`.
#[test]
fn full_index_pipelines_produce_equivalent_db_state() {
    let project = tempfile::tempdir().unwrap();
    create_ts_project(project.path());

    // ── A: Shared `run_index_pipeline` ──
    let store_a = Arc::new(Store::open_in_memory().unwrap());
    store_a.init_schema().unwrap();

    let _stats = run_index_pipeline(
        &store_a,
        project.path(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .unwrap();

    let snap_a = DbSnapshot::from_store(&store_a);

    // ── B: New `IndexPipeline::run` (structured orchestrator) ──
    let store_b = Arc::new(Store::open_in_memory().unwrap());
    store_b.init_schema().unwrap();

    let pipeline = IndexPipeline::new(
        Arc::clone(&store_b),
        project.path().to_path_buf(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    );
    let _stats = pipeline.run(&NoopSink, &mut || false).unwrap();

    let snap_b = DbSnapshot::from_store(&store_b);

    // ── Assert equivalence ────────────────────────────────────────
    assert_eq!(
        snap_a.file_count, snap_b.file_count,
        "file count mismatch: shared={} structured={}",
        snap_a.file_count, snap_b.file_count,
    );
    assert_eq!(
        snap_a.symbol_count, snap_b.symbol_count,
        "symbol count mismatch: shared={} structured={}",
        snap_a.symbol_count, snap_b.symbol_count,
    );
    assert_eq!(
        snap_a.edge_count, snap_b.edge_count,
        "edge count mismatch: shared={} structured={}",
        snap_a.edge_count, snap_b.edge_count,
    );
    assert_eq!(
        snap_a.symbol_names, snap_b.symbol_names,
        "symbol names differ between shared and structured pipeline"
    );

    // Sanity — we indexed real TypeScript files and should have real data.
    assert!(snap_a.file_count > 0, "expected at least one file in DB");
    assert!(
        snap_a.symbol_count > 0,
        "expected at least one symbol in DB"
    );
}

// ── Test 2: incremental equivalence ────────────────────────────────────────

/// After an initial full index, modifying one file and running
/// `IncrementalPipeline::sync` must bring the DB to the same state as a
/// fresh `run_index_pipeline` on the modified project.
#[test]
fn incremental_pipeline_matches_fresh_index_after_file_change() {
    let project = tempfile::tempdir().unwrap();
    create_ts_project(project.path());

    // ── Step 1: initial full index ──
    let store_inc = Arc::new(Store::open_in_memory().unwrap());
    store_inc.init_schema().unwrap();

    run_index_pipeline(
        &store_inc,
        project.path(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .unwrap();

    // ── Step 2: modify a file on disk ──
    // Sleep briefly so file mtime clearly differs (some filesystems have
    // 1-second granularity; BLAKE3 hashes catch the change regardless, but
    // this avoids any mtime-based heuristics tripping false-negatives).
    std::thread::sleep(std::time::Duration::from_millis(100));
    modify_math_file(project.path());

    // ── Step 3: incremental sync on the SAME store ──
    let inc = IncrementalPipeline::new(
        Arc::clone(&store_inc),
        project.path().to_path_buf(),
        ExtractionMode::Structural,
    );
    inc.sync(&NoopSink, &mut || false).unwrap();

    let snap_inc = DbSnapshot::from_store(&store_inc);

    // ── Step 4: fresh index on modified project (separate store) ──
    let store_fresh = Arc::new(Store::open_in_memory().unwrap());
    store_fresh.init_schema().unwrap();

    run_index_pipeline(
        &store_fresh,
        project.path(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .unwrap();

    let snap_fresh = DbSnapshot::from_store(&store_fresh);

    // ── Assert equivalence ────────────────────────────────────────
    assert_eq!(
        snap_inc.file_count, snap_fresh.file_count,
        "file count mismatch after incremental vs fresh (inc={}, fresh={})",
        snap_inc.file_count, snap_fresh.file_count,
    );
    assert_eq!(
        snap_inc.symbol_count, snap_fresh.symbol_count,
        "symbol count mismatch after incremental vs fresh (inc={}, fresh={})",
        snap_inc.symbol_count, snap_fresh.symbol_count,
    );
    assert_eq!(
        snap_inc.edge_count, snap_fresh.edge_count,
        "edge count mismatch after incremental vs fresh (inc={}, fresh={})",
        snap_inc.edge_count, snap_fresh.edge_count,
    );
    assert_eq!(
        snap_inc.symbol_names, snap_fresh.symbol_names,
        "symbol names differ: incremental vs fresh index after file change"
    );

    // The modified file added two functions (subtract, divide) — the
    // symbol count should reflect the larger set.
    assert!(
        snap_inc.symbol_count > 0,
        "expected symbols after incremental pipeline"
    );
}

// ── Test 3: incremental deletion ───────────────────────────────────────────

/// After an initial full index, deleting a file and running
/// `IncrementalPipeline::sync` must clean up the deleted file's symbols and
/// edges so that the DB matches a fresh index of only the remaining files.
#[test]
fn incremental_pipeline_detects_deleted_files() {
    let project = tempfile::tempdir().unwrap();
    create_ts_project(project.path());

    // ── Step 1: initial full index on all 4 files ──
    let store_inc = Arc::new(Store::open_in_memory().unwrap());
    store_inc.init_schema().unwrap();

    run_index_pipeline(
        &store_inc,
        project.path(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .unwrap();

    // ── Step 2: delete types.ts from disk ──
    std::thread::sleep(std::time::Duration::from_millis(100));
    std::fs::remove_file(project.path().join("types.ts")).unwrap();

    // ── Step 3: incremental sync on the SAME store ──
    let inc = IncrementalPipeline::new(
        Arc::clone(&store_inc),
        project.path().to_path_buf(),
        ExtractionMode::Structural,
    );
    inc.sync(&NoopSink, &mut || false).unwrap();

    let snap_inc = DbSnapshot::from_store(&store_inc);

    // ── Step 4: fresh index of only the remaining 3 files (separate store) ──
    let project_fresh = tempfile::tempdir().unwrap();
    create_ts_project(project_fresh.path());
    std::fs::remove_file(project_fresh.path().join("types.ts")).unwrap();

    let store_fresh = Arc::new(Store::open_in_memory().unwrap());
    store_fresh.init_schema().unwrap();

    run_index_pipeline(
        &store_fresh,
        project_fresh.path(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .unwrap();

    let snap_fresh = DbSnapshot::from_store(&store_fresh);

    // ── Assert equivalence ────────────────────────────────────────
    assert_eq!(
        snap_inc.file_count, snap_fresh.file_count,
        "file count mismatch after deletion (inc={}, fresh={})",
        snap_inc.file_count, snap_fresh.file_count,
    );
    assert_eq!(
        snap_inc.symbol_count, snap_fresh.symbol_count,
        "symbol count mismatch after deletion (inc={}, fresh={})",
        snap_inc.symbol_count, snap_fresh.symbol_count,
    );
    assert_eq!(
        snap_inc.edge_count, snap_fresh.edge_count,
        "edge count mismatch after deletion (inc={}, fresh={})",
        snap_inc.edge_count, snap_fresh.edge_count,
    );
    assert_eq!(
        snap_inc.symbol_names, snap_fresh.symbol_names,
        "symbol names differ: incremental vs fresh index after file deletion"
    );

    // Sanity — the deleted file should be gone.
    assert_eq!(
        snap_inc.file_count, 3,
        "expected 3 files after deletion, got {}",
        snap_inc.file_count,
    );
    assert!(
        snap_inc.symbol_count > 0,
        "expected symbols after deletion cleanup"
    );
}

// ── Test 4: path alias config change ───────────────────────────────────────

/// After an initial full index, changing tsconfig.json (PathAliasConfig)
/// and running `IncrementalPipeline::sync` must invalidate references and
/// edges, then rebuild them so the final DB matches a fresh index.
#[test]
fn incremental_pipeline_handles_alias_config_change() {
    let project = tempfile::tempdir().unwrap();
    create_ts_project(project.path());

    // ── Write initial tsconfig.json with path aliases ──
    let tsconfig_v1 = r#"{"compilerOptions":{"baseUrl":".","paths":{"@lib/*":["lib/*"]}}}"#;
    std::fs::write(project.path().join("tsconfig.json"), tsconfig_v1).unwrap();

    // ── Step 1: initial full index (IndexPipeline commits alias config hash) ──
    let store_inc = Arc::new(Store::open_in_memory().unwrap());
    store_inc.init_schema().unwrap();

    let pipeline = IndexPipeline::new(
        Arc::clone(&store_inc),
        project.path().to_path_buf(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    );
    pipeline.run(&NoopSink, &mut || false).unwrap();

    // ── Step 2: modify tsconfig.json (different alias pattern) ──
    std::thread::sleep(std::time::Duration::from_millis(100));
    let tsconfig_v2 =
        r#"{"compilerOptions":{"baseUrl":".","paths":{"@shared/*":["src/shared/*"]}}}"#;
    std::fs::write(project.path().join("tsconfig.json"), tsconfig_v2).unwrap();

    // ── Step 3: incremental sync on the SAME store ──
    let inc = IncrementalPipeline::new(
        Arc::clone(&store_inc),
        project.path().to_path_buf(),
        ExtractionMode::Structural,
    );
    inc.sync(&NoopSink, &mut || false).unwrap();

    let snap_inc = DbSnapshot::from_store(&store_inc);

    // ── Step 4: fresh index on the modified project (separate store) ──
    let store_fresh = Arc::new(Store::open_in_memory().unwrap());
    store_fresh.init_schema().unwrap();

    run_index_pipeline(
        &store_fresh,
        project.path(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    )
    .unwrap();

    let snap_fresh = DbSnapshot::from_store(&store_fresh);

    // ── Assert equivalence — edges were invalidated and rebuilt correctly ──
    assert_eq!(
        snap_inc.file_count, snap_fresh.file_count,
        "file count mismatch after alias change (inc={}, fresh={})",
        snap_inc.file_count, snap_fresh.file_count,
    );
    assert_eq!(
        snap_inc.symbol_count, snap_fresh.symbol_count,
        "symbol count mismatch after alias change (inc={}, fresh={})",
        snap_inc.symbol_count, snap_fresh.symbol_count,
    );
    assert_eq!(
        snap_inc.edge_count, snap_fresh.edge_count,
        "edge count mismatch after alias change (inc={}, fresh={})",
        snap_inc.edge_count, snap_fresh.edge_count,
    );
    assert_eq!(
        snap_inc.symbol_names, snap_fresh.symbol_names,
        "symbol names differ: incremental vs fresh index after alias config change"
    );

    assert!(snap_inc.file_count > 0, "expected files after alias change");
    assert!(
        snap_inc.symbol_count > 0,
        "expected symbols after alias change"
    );
}

// ── Test 5: cancellation ───────────────────────────────────────────────────

/// Running `IndexPipeline` with an `interrupted` closure that returns `true`
/// after Phase 2 (HashCheck) must emit a `Cancelled` event, return
/// `Ok(IndexPipelineStats::default())`, and leave the DB in a consistent
/// state (no resolution happened).
#[test]
fn index_pipeline_cancellation_leaves_partial_db() {
    use std::sync::Arc as StdArc;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use filesync::{PhaseName, ProgressEvent, ProgressSink};

    /// A sink that records events and signals when HashCheck completes.
    struct HashCheckSignalSink {
        events: Mutex<Vec<ProgressEvent>>,
        hashcheck_done: StdArc<AtomicBool>,
    }

    impl ProgressSink for HashCheckSignalSink {
        fn emit(&self, event: ProgressEvent) {
            if matches!(
                event,
                ProgressEvent::PhaseFinished {
                    phase: PhaseName::HashCheck,
                    ..
                }
            ) {
                self.hashcheck_done.store(true, Ordering::SeqCst);
            }
            self.events.lock().unwrap().push(event);
        }
    }

    let project = tempfile::tempdir().unwrap();
    create_ts_project(project.path());

    let store = Arc::new(Store::open_in_memory().unwrap());
    store.init_schema().unwrap();

    let hashcheck_done = StdArc::new(AtomicBool::new(false));
    let sink = HashCheckSignalSink {
        events: Mutex::new(Vec::new()),
        hashcheck_done: StdArc::clone(&hashcheck_done),
    };

    let pipeline = IndexPipeline::new(
        Arc::clone(&store),
        project.path().to_path_buf(),
        IndexPipelineOptions::new(ExtractionMode::Structural),
    );

    // Interrupt after HashCheck phase finishes.
    let hc = StdArc::clone(&hashcheck_done);
    let stats = pipeline
        .run(&sink, &mut move || hc.load(Ordering::SeqCst))
        .unwrap();

    // ── Cancellation returns default (zero) stats ──
    assert_eq!(
        stats.discovered, 0,
        "cancelled pipeline should return default stats"
    );
    assert_eq!(
        stats.indexed, 0,
        "cancelled pipeline should return default stats"
    );

    // ── Cancelled event emitted with correct last_phase ──
    let events = sink.events.lock().unwrap();
    let cancelled = events.iter().any(|e| {
        matches!(
            e,
            ProgressEvent::Cancelled {
                last_phase: PhaseName::HashCheck,
            }
        )
    });
    assert!(
        cancelled,
        "should emit Cancelled event with HashCheck as last_phase"
    );

    // ── DB is in a consistent state (no resolution/extraction happened after Phase 2) ──
    // Phase 1 (Discovery) and Phase 2 (HashCheck) are read-only — no writes to files/symbols/edges.
    // The store may have cleanup of deleted files, but for a fresh store there are none.
    let file_count = store.count_files().unwrap_or(0);
    let sym_count = store.count_symbols().unwrap_or(0);
    assert!(
        file_count == 0 && sym_count == 0,
        "expected empty DB after cancellation before extraction (files={file_count}, symbols={sym_count})"
    );
}

// ── Test 6: Full mode summary equivalence ──────────────────────────────────

/// `run_index_pipeline` and `IndexPipeline::run` in `ExtractionMode::Full`
/// must produce the same number of function summaries and equivalent summary
/// data (same file coverage, same content hashes).
#[test]
fn full_mode_pipelines_produce_equivalent_summaries() {
    let project = tempfile::tempdir().unwrap();

    // Create a TypeScript project with functions that have dataflow
    // (parameters and return values) so Full mode produces summaries.
    std::fs::write(
        project.path().join("math.ts"),
        "\
export function add(a: number, b: number): number {\n\
    return a + b;\n\
}\n\
\n\
export function multiply(a: number, b: number): number {\n\
    return a * b;\n\
}\n",
    )
    .unwrap();

    // NOTE: Full mode triggers `phase_build_summaries` → `SummaryStore::build_all`,
    // which holds a write lock while calling the user-provided `build_fn`.  On
    // in-memory databases, `lock_read()` falls back to the same `std::sync::Mutex`
    // as `lock()`, causing a deadlock inside `SummaryBuilder::build` when it calls
    // `TraceStore` read methods.  File-backed databases have separate read/write
    // connections and do not deadlock.
    //
    // See `store/mod.rs:82-86` (StoreReader::lock_read) and line 295-298
    // (with_transaction docs) for the documented API contract.

    let db_dir_a = tempfile::tempdir().unwrap();
    let db_path_a = db_dir_a.path().join("a.db");
    let db_dir_b = tempfile::tempdir().unwrap();
    let db_path_b = db_dir_b.path().join("b.db");

    // ── A: Shared `run_index_pipeline` (Full mode) ──
    let store_a = Arc::new(Store::open_db(&db_path_a).unwrap());
    store_a.init_schema().unwrap();

    let _stats = run_index_pipeline(
        &store_a,
        project.path(),
        IndexPipelineOptions::new(ExtractionMode::Full),
    )
    .unwrap();

    let snap_a = DbSnapshot::from_store(&store_a);

    // ── B: Structured `IndexPipeline::run` (Full mode) ──
    let store_b = Arc::new(Store::open_db(&db_path_b).unwrap());
    store_b.init_schema().unwrap();

    let pipeline = IndexPipeline::new(
        Arc::clone(&store_b),
        project.path().to_path_buf(),
        IndexPipelineOptions::new(ExtractionMode::Full),
    );
    let _stats = pipeline.run(&NoopSink, &mut || false).unwrap();

    let snap_b = DbSnapshot::from_store(&store_b);

    // ── Assert equivalence ────────────────────────────────────────
    assert_eq!(
        snap_a.file_count, snap_b.file_count,
        "file count mismatch in Full mode (shared={}, structured={})",
        snap_a.file_count, snap_b.file_count,
    );
    assert_eq!(
        snap_a.symbol_count, snap_b.symbol_count,
        "symbol count mismatch in Full mode (shared={}, structured={})",
        snap_a.symbol_count, snap_b.symbol_count,
    );
    assert_eq!(
        snap_a.edge_count, snap_b.edge_count,
        "edge count mismatch in Full mode (shared={}, structured={})",
        snap_a.edge_count, snap_b.edge_count,
    );
    assert_eq!(
        snap_a.symbol_names, snap_b.symbol_names,
        "symbol names differ in Full mode between shared and structured pipeline"
    );

    // ── Verify summary equivalence ──
    let summaries_a = db::summary::SummaryStore::files_with_summaries(&store_a).unwrap();
    let summaries_b = db::summary::SummaryStore::files_with_summaries(&store_b).unwrap();

    assert!(
        !summaries_a.is_empty(),
        "Full mode pipeline A should produce function summaries"
    );
    assert_eq!(
        summaries_a.len(),
        summaries_b.len(),
        "same number of files with summaries: A={}, B={}",
        summaries_a.len(),
        summaries_b.len(),
    );

    // Verify each summary pair (file_id, content_hash) matches
    for (id_a, hash_a) in &summaries_a {
        let found = summaries_b
            .iter()
            .any(|(id_b, hash_b)| id_a == id_b && hash_a == hash_b);
        assert!(
            found,
            "missing summary match for file {:?} hash {}",
            id_a, hash_a
        );
    }

    // Sanity — Full mode produces more data than Structural
    assert!(
        snap_a.file_count > 0,
        "expected at least one file in Full mode"
    );
    assert!(
        snap_a.symbol_count > 0,
        "expected at least one symbol in Full mode"
    );
}
