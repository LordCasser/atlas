//! Handler purity ratchet (DEBT-8 foundation).
//!
//! Scans handler source under `src/tools/` for direct service-construction /
//! Anti-Pattern strings that should go through runtime modules / dispatcher.
//!
//! **Not perfect**: helpers can still hide direct calls. The allowlist only
//! shrinks; new hits fail the test. Full migration is gradual (Task 10).

#![cfg(test)]

use std::collections::BTreeSet;
use std::path::PathBuf;

/// Forbidden substrings (runtime Anti-Patterns + heavy analysis types).
const FORBIDDEN: &[&str] = &[
    "FieldLifecycleEngine::",
    "BranchDiffEngine::",
    "BranchDiffSemantic::",
    "CrossFunctionBridge::",
    ".lazy_service()",
    "materialize.dataflow().ensure_for_function",
    "materialize.dataflow().ensure_for_position",
    "LazyDataflowService::with_structural_rebuilder",
    "FocusMaterialize::open",
    "cache.has_manual_full_index()",
    "focus_runtime.lock()",
    "store.upsert_fp_annotation(",
    "store.upsert_domain_rule(",
];

/// Paths relative to `crates/atlas-mcp/src/tools/` that may still contain hits
/// during migration. **Only shrink.** Empty = fully pure.
const ALLOWLIST: &[&str] = &[
    // Analysis tools still construct engines directly (dispatch_analysis target).
    "lifecycle.rs",
    "branch_diff.rs",
    // Graph handlers embed analysis and ensure paths (dispatch_graph_query target).
    "graph.rs",
    // God-router still owns prepare/refresh/orchestration until sub-dispatchers land.
    "mod.rs",
    // Overlay handlers until OverlayMutation is fully dispatcher-owned.
    "overlay.rs",
    "annotations.rs",
    // Resume still drives refresh/orchestration.
    "resume.rs",
    // Project open must construct FocusMaterialize once (factory, not per-request).
    "active_project.rs",
];

fn tools_src_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/tools")
}

fn scan_file(path: &std::path::Path, rel: &str) -> Vec<(String, usize, String)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return vec![];
    };
    let mut hits = Vec::new();
    for (line_no, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.starts_with("//") || trimmed.starts_with("//!") || trimmed.starts_with('*') {
            continue;
        }
        for pat in FORBIDDEN {
            if line.contains(pat) {
                hits.push((rel.to_string(), line_no + 1, (*pat).to_string()));
            }
        }
    }
    hits
}

fn collect_rs_files(dir: &std::path::Path, prefix: &str, out: &mut Vec<(PathBuf, String)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in entries.flatten() {
        let path = ent.path();
        let name = ent.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            let next = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            collect_rs_files(&path, &next, out);
        } else if name.ends_with(".rs") {
            let rel = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            // Skip purity test itself and pure runtime modules (they are the target).
            if rel == "handler_purity.rs" || rel.starts_with("runtime/") || rel == "tool_contract.rs"
            {
                continue;
            }
            out.push((path, rel));
        }
    }
}

#[test]
fn handler_purity_no_new_direct_service_calls() {
    let root = tools_src_dir();
    assert!(
        root.is_dir(),
        "tools source dir missing: {}",
        root.display()
    );

    let allow: BTreeSet<&str> = ALLOWLIST.iter().copied().collect();
    let mut files = Vec::new();
    collect_rs_files(&root, "", &mut files);

    let mut violations = Vec::new();
    let mut allowlist_unused: BTreeSet<&str> = allow.clone();

    for (path, rel) in &files {
        let hits = scan_file(path, rel);
        if hits.is_empty() {
            continue;
        }
        if allow.contains(rel.as_str()) {
            allowlist_unused.remove(rel.as_str());
            continue;
        }
        for (r, line, pat) in hits {
            violations.push(format!("{r}:{line}: forbidden `{pat}`"));
        }
    }

    assert!(
        violations.is_empty(),
        "handler purity violations (route via dispatcher/runtime; or shrink ALLOWLIST only after fixing):\n{}",
        violations.join("\n")
    );

    // Optional: warn via assert that allowlist entries still have hits?
    // Keep allowlist entries that still need migration; empty unused is OK if file cleaned.
    let _ = allowlist_unused;
}

#[test]
fn handler_purity_allowlist_only_shrinks_documented() {
    // Guard: allowlist must not grow without intentional edit of this test's
    // constant. Count is the ratchet baseline for DEBT-8 migration.
    assert!(
        ALLOWLIST.len() <= 8,
        "ALLOWLIST grew beyond baseline (8); migration should only shrink it"
    );
}
