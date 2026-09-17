//! Real-process regression for capability upgrades through `atlas sync`.
//!
//! The oracle is the persistent SQLite state reopened after each process exits;
//! terminal progress text and timing are intentionally not part of the contract.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use rusqlite::Connection;

fn atlas_binary() -> &'static str {
    env!("CARGO_BIN_EXE_atlas")
}

fn run_atlas(project: &Path, args: &[&str]) -> Output {
    let output = Command::new(atlas_binary())
        .args(args)
        .arg("--project")
        .arg(project)
        .output()
        .expect("atlas process must start");
    assert!(
        output.status.success(),
        "atlas {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

#[derive(Debug, PartialEq, Eq)]
struct PersistentFacts {
    grade: String,
    file_count: i64,
    edge_count: i64,
    summary_count: i64,
    layers: BTreeMap<String, i64>,
    last_sync_time: Option<String>,
}

fn persistent_facts(project: &Path) -> PersistentFacts {
    let conn = Connection::open(project.join(".atlas/atlas.db"))
        .expect("atlas must create a persistent database");
    let grade = conn
        .query_row(
            "SELECT value FROM project_metadata WHERE key = 'indexed_pipeline_grade'",
            [],
            |row| row.get(0),
        )
        .expect("pipeline grade must exist");
    let file_count = conn
        .query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))
        .unwrap();
    let edge_count = conn
        .query_row("SELECT COUNT(*) FROM symbol_edges", [], |row| row.get(0))
        .unwrap();
    let summary_count = conn
        .query_row("SELECT COUNT(*) FROM function_summaries", [], |row| {
            row.get(0)
        })
        .unwrap();
    let last_sync_time = conn
        .query_row(
            "SELECT value FROM project_metadata WHERE key = 'last_sync_time'",
            [],
            |row| row.get(0),
        )
        .ok();

    let mut stmt = conn
        .prepare(
            "SELECT layer, COUNT(*) FROM extraction_state \
             WHERE unit_id IS NULL AND status = 'complete' GROUP BY layer ORDER BY layer",
        )
        .unwrap();
    let layers = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<BTreeMap<String, i64>, _>>()
        .unwrap();

    PersistentFacts {
        grade,
        file_count,
        edge_count,
        summary_count,
        layers,
        last_sync_time,
    }
}

#[test]
fn sync_upgrades_unchanged_persistent_index_from_manifest_to_full() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("math.ts"),
        "export function add(a: number, b: number): number { return a + b; }\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("main.ts"),
        "import { add } from './math';\n\
         export function main(): number { return add(1, 2); }\n",
    )
    .unwrap();

    run_atlas(project.path(), &["index", "--analysis", "manifest"]);
    let manifest = persistent_facts(project.path());
    assert_eq!(manifest.grade, "manifest");
    assert_eq!(manifest.file_count, 2);
    assert_eq!(manifest.layers.get("manifest"), Some(&2));
    assert_eq!(manifest.edge_count, 0);
    assert_eq!(manifest.summary_count, 0);
    assert!(manifest.last_sync_time.is_none());

    run_atlas(project.path(), &["sync", "--analysis", "structural"]);
    let structural = persistent_facts(project.path());
    assert_eq!(structural.grade, "structural");
    assert_eq!(structural.file_count, 2);
    assert_eq!(structural.layers.get("structural"), Some(&2));
    assert!(structural.edge_count > 0);
    assert_eq!(structural.summary_count, 0);
    assert!(structural.last_sync_time.is_some());

    run_atlas(project.path(), &["sync", "--analysis", "full"]);
    let full = persistent_facts(project.path());
    assert_eq!(full.grade, "full");
    assert_eq!(full.file_count, 2);
    assert_eq!(full.layers.get("dataflow"), Some(&2));
    assert!(full.edge_count > 0);
    assert!(full.summary_count > 0);
    assert!(full.last_sync_time.is_some());

    run_atlas(project.path(), &["sync", "--analysis", "full"]);
    assert_eq!(persistent_facts(project.path()), full);
}
