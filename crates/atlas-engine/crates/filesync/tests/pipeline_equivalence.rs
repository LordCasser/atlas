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
    run_index_pipeline, IndexPipeline, IndexPipelineOptions, IncrementalPipeline, NoopSink,
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
    assert!(snap_a.symbol_count > 0, "expected at least one symbol in DB");
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
