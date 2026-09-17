//! Real-process regressions for `atlas sync` inheriting persisted index scope.
//!
//! Each command exits before SQLite is reopened. Persistent facts are the
//! oracle; terminal progress text and phase timing are intentionally ignored.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Output};

use rusqlite::Connection;

fn atlas_binary() -> &'static str {
    env!("CARGO_BIN_EXE_atlas")
}

fn run_atlas_raw(project: &Path, args: &[&str]) -> Output {
    Command::new(atlas_binary())
        .args(args)
        .arg("--project")
        .arg(project)
        .output()
        .expect("atlas process must start")
}

fn run_atlas(project: &Path, args: &[&str]) {
    let output = run_atlas_raw(project, args);
    assert!(
        output.status.success(),
        "atlas {args:?} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[derive(Debug, PartialEq, Eq)]
struct PersistentFacts {
    scope: String,
    grade: String,
    paths: Vec<String>,
    edge_count: i64,
    summary_count: i64,
    layers: BTreeMap<String, i64>,
    last_sync_time: Option<String>,
}

fn persistent_facts(project: &Path) -> PersistentFacts {
    let conn = Connection::open(project.join(".atlas/atlas.db"))
        .expect("atlas must create a persistent database");
    let metadata = |key: &str| {
        conn.query_row(
            "SELECT value FROM project_metadata WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .ok()
    };

    let mut path_stmt = conn
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap();
    let paths = path_stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<Vec<String>, _>>()
        .unwrap();
    let edge_count = conn
        .query_row("SELECT COUNT(*) FROM symbol_edges", [], |row| row.get(0))
        .unwrap();
    let summary_count = conn
        .query_row("SELECT COUNT(*) FROM function_summaries", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut layer_stmt = conn
        .prepare(
            "SELECT layer, COUNT(*) FROM extraction_state \
             WHERE unit_id IS NULL AND status = 'complete' GROUP BY layer ORDER BY layer",
        )
        .unwrap();
    let layers = layer_stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<BTreeMap<String, i64>, _>>()
        .unwrap();

    PersistentFacts {
        scope: metadata("indexed_scope").expect("indexed_scope must exist"),
        grade: metadata("indexed_pipeline_grade").expect("pipeline grade must exist"),
        paths,
        edge_count,
        summary_count,
        layers,
        last_sync_time: metadata("last_sync_time"),
    }
}

fn write_fixture(project: &Path) {
    std::fs::create_dir_all(project.join("src/generated")).unwrap();
    std::fs::create_dir_all(project.join("other")).unwrap();
    std::fs::write(
        project.join("src/main.ts"),
        "import { add } from './math';\nexport function main(): number { return add(1, 2); }\n",
    )
    .unwrap();
    std::fs::write(
        project.join("src/math.ts"),
        "export function add(a: number, b: number): number { return a + b; }\n",
    )
    .unwrap();
    std::fs::write(
        project.join("src/stale.ts"),
        "export function stale(): number { return 0; }\n",
    )
    .unwrap();
    std::fs::write(
        project.join("src/generated/code.ts"),
        "export function generated(): number { return 3; }\n",
    )
    .unwrap();
    std::fs::write(
        project.join("other/dep.ts"),
        "export function outside(): number { return 4; }\n",
    )
    .unwrap();
}

#[test]
fn sync_preserves_persisted_scope_across_changes_and_capability_upgrades() {
    let project = tempfile::tempdir().unwrap();
    write_fixture(project.path());

    run_atlas(
        project.path(),
        &[
            "index",
            "--include",
            "src/**",
            "--exclude",
            "src/generated/**",
            "--analysis",
            "manifest",
        ],
    );
    let manifest = persistent_facts(project.path());
    assert_eq!(manifest.grade, "manifest");
    assert_eq!(
        manifest.paths,
        ["src/main.ts", "src/math.ts", "src/stale.ts"]
    );
    assert_eq!(manifest.layers.get("manifest"), Some(&3));
    assert_eq!(manifest.edge_count, 0);
    assert_eq!(manifest.summary_count, 0);
    assert!(manifest.last_sync_time.is_none());

    std::fs::write(
        project.path().join("src/main.ts"),
        "import { add } from './math';\nexport function main(): number { return add(2, 3); }\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("src/new.ts"),
        "export function fresh(): number { return 5; }\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("src/generated/new.ts"),
        "export function generatedAgain(): number { return 6; }\n",
    )
    .unwrap();
    std::fs::write(
        project.path().join("other/new.ts"),
        "export function outsideAgain(): number { return 7; }\n",
    )
    .unwrap();
    std::fs::remove_file(project.path().join("src/stale.ts")).unwrap();

    run_atlas(project.path(), &["sync", "--analysis", "structural"]);
    let structural = persistent_facts(project.path());
    assert_eq!(structural.scope, manifest.scope);
    assert_eq!(structural.grade, "structural");
    assert_eq!(
        structural.paths,
        ["src/main.ts", "src/math.ts", "src/new.ts"]
    );
    assert_eq!(structural.layers.get("structural"), Some(&3));
    assert!(structural.edge_count > 0);
    assert_eq!(structural.summary_count, 0);
    assert!(structural.last_sync_time.is_some());

    run_atlas(project.path(), &["sync", "--analysis", "full"]);
    let full = persistent_facts(project.path());
    assert_eq!(full.scope, manifest.scope);
    assert_eq!(full.grade, "full");
    assert_eq!(full.paths, structural.paths);
    assert_eq!(full.layers.get("dataflow"), Some(&3));
    assert!(full.edge_count > 0);
    assert!(full.summary_count > 0);
    assert!(full.last_sync_time.is_some());
    assert!(project.path().join("src/generated/new.ts").exists());
    assert!(project.path().join("other/new.ts").exists());

    run_atlas(project.path(), &["sync", "--analysis", "full"]);
    assert_eq!(persistent_facts(project.path()), full);
}

#[test]
fn sync_fails_closed_when_persisted_scope_is_malformed() {
    let project = tempfile::tempdir().unwrap();
    write_fixture(project.path());
    run_atlas(
        project.path(),
        &["index", "--include", "src/**", "--analysis", "structural"],
    );

    let before = persistent_facts(project.path());
    let conn = Connection::open(project.path().join(".atlas/atlas.db")).unwrap();
    conn.execute(
        "UPDATE project_metadata SET value = '[]' WHERE key = 'indexed_scope'",
        [],
    )
    .unwrap();
    drop(conn);

    let output = run_atlas_raw(project.path(), &["sync", "--analysis", "structural"]);
    assert!(!output.status.success(), "malformed scope must fail closed");

    let after = persistent_facts(project.path());
    assert_eq!(after.scope, "[]");
    assert_eq!(after.grade, before.grade);
    assert_eq!(after.paths, before.paths);
    assert_eq!(after.edge_count, before.edge_count);
    assert_eq!(after.summary_count, before.summary_count);
    assert_eq!(after.layers, before.layers);
    assert_eq!(after.last_sync_time, before.last_sync_time);
}
