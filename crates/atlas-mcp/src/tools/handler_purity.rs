//! Handler purity ratchet (DEBT-8).
//!
//! Scans handler source under `src/tools/` for direct service-construction /
//! Anti-Pattern strings that should go through runtime modules / dispatcher.
//!
//! **Not perfect**: helpers can still hide direct calls. The allowlist only
//! shrinks; new hits fail the test.
//!
//! Analysis engines (`FieldLifecycleEngine` / `BranchDiffEngine`) are migrated
//! to `runtime/analysis_runtime.rs` (Task 10). Residual allowlist is non-analysis
//! only: god-router locks, annotation test seeds, project-open factory.

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
    // God-router still owns prepare/refresh/orchestration until sub-dispatchers land.
    "mod.rs",
    // annotations tests still seed via store.upsert_fp_annotation; production path
    // already uses overlay_runtime.
    "annotations.rs",
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

    // Residual allowlist must still have real hits — drop dead entries, don't leave
    // vestigial paths that no longer violate (ratchet hygiene).
    assert!(
        allowlist_unused.is_empty(),
        "ALLOWLIST entries with no forbidden hits (remove them):\n{}",
        allowlist_unused.into_iter().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn handler_purity_allowlist_only_shrinks_documented() {
    // Guard: allowlist must not grow without intentional edit of this test's
    // constant. Foundation baseline was 8; only shrink thereafter.
    assert!(
        ALLOWLIST.len() <= 8,
        "ALLOWLIST grew beyond DEBT-8 foundation baseline (8); migration should only shrink it"
    );
    // Analysis handlers must stay off the allowlist once migrated (Task 10).
    for banned in ["lifecycle.rs", "branch_diff.rs", "graph.rs"] {
        assert!(
            !ALLOWLIST.contains(&banned),
            "analysis handler {banned} must not be on purity allowlist after DEBT-8 migration"
        );
    }
    // Current residual ceiling (non-analysis factory/test seeds + god-router).
    assert!(
        ALLOWLIST.len() <= 3,
        "ALLOWLIST grew beyond residual baseline (3); only shrink"
    );
}

#[test]
fn handler_purity_analysis_handlers_have_no_engine_hits() {
    let root = tools_src_dir();
    for rel in ["lifecycle.rs", "branch_diff.rs", "graph.rs"] {
        let path = root.join(rel);
        let hits = scan_file(&path, rel);
        let engine_hits: Vec<_> = hits
            .into_iter()
            .filter(|(_, _, pat)| {
                pat == "FieldLifecycleEngine::"
                    || pat == "BranchDiffEngine::"
                    || pat == "BranchDiffSemantic::"
            })
            .collect();
        assert!(
            engine_hits.is_empty(),
            "{rel} must not call lifecycle/branch-diff engines by name (route via AnalysisRuntime):\n{:?}",
            engine_hits
        );
    }
}

/// DEBT-8 substance: analysis tool handlers must not own store/composition
/// orchestration (that lives on AnalysisRuntime). Engine-name purity alone is
/// insufficient — catch the old facade regression.
#[test]
fn handler_purity_analysis_tools_no_orchestration_in_handlers() {
    const ORCH: &[&str] = &[
        "find_cfg_nodes_by_function",
        "find_cfg_edges_by_function",
        "find_data_nodes_by_function",
        "find_dataflow_edges_by_sources",
        "compose_effects(",
        "CfgGraph::build",
        "CppOwnershipRules::load_for",
        "ResourceOpConfig::default_for",
    ];
    let root = tools_src_dir();
    // lifecycle + branch_diff must be fully dispatcher-owned. graph impact may
    // still load CFG from the store for multi-node explore; it must not rebuild
    // composition itself (uses analysis_runtime.semantic_composition_for_function).
    for rel in ["lifecycle.rs", "branch_diff.rs"] {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path).expect(rel);
        for pat in ORCH {
            for (i, line) in text.lines().enumerate() {
                let t = line.trim();
                if t.starts_with("//") || t.starts_with("//!") {
                    continue;
                }
                assert!(
                    !line.contains(pat),
                    "{rel}:{}: analysis tool must not own `{pat}` (move to AnalysisRuntime):\n  {line}",
                    i + 1
                );
            }
        }
    }
    // graph: composition must go through runtime helpers, not compose_effects/CfgGraph.
    let graph = std::fs::read_to_string(root.join("graph.rs")).expect("graph.rs");
    for pat in ["compose_effects(", "CfgGraph::build", "FieldLifecycleEngine::", "BranchDiffEngine::"]
    {
        for (i, line) in graph.lines().enumerate() {
            let t = line.trim();
            if t.starts_with("//") || t.starts_with("//!") {
                continue;
            }
            assert!(
                !line.contains(pat),
                "graph.rs:{}: forbidden analysis orchestration `{pat}`:\n  {line}",
                i + 1
            );
        }
    }
}
