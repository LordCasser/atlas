//! `atlas taint` — run taint analysis (source→sink propagation).
//!
//! Loads taint rules (default + user), data nodes, and dataflow edges;
//! runs forward propagation to detect source→sink flows; traces paths.

use crate::analysis::taint::{TaintEngine, TaintPathTracer, TaintRuleLoader};
use crate::db::Store;
use crate::types::ids::FileId;
use crate::types::taint::Severity;
use anyhow::Context;
use std::path::Path;

pub fn run(
    project: &str,
    file_id_hex: Option<&str>,
    severity: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let root = Path::new(project);

    let store = Store::open(root).context("Failed to open Atlas database")?;

    // ── 1. Load taint rules ────────────────────────────────────────────────
    if !json {
        eprintln!("Loading taint rules...");
    }
    let languages = get_indexed_languages(&store)?;
    let mut rules = TaintRuleLoader::load_defaults(&languages);

    // Load user rules from .atlas/rules/ (if any)
    let user_rules_dir = root.join(".atlas").join("rules");
    if user_rules_dir.is_dir() {
        match TaintRuleLoader::load_user_rules(&user_rules_dir) {
            Ok(user) => {
                if !json {
                    eprintln!("  Loaded {} user-defined rules", user.len());
                }
                // Override: user rules replace default rules with same (language, kind, callee, symbol_pattern)
                for ur in user {
                    rules.retain(|r| {
                        !(r.language == ur.language
                            && r.kind == ur.kind
                            && r.callee == ur.callee
                            && r.symbol_pattern == ur.symbol_pattern)
                    });
                    rules.push(ur);
                }
            }
            Err(e) => eprintln!("  Warning: failed to load user rules: {}", e),
        }
    }

    if rules.is_empty() {
        eprintln!("No taint rules available. Add rules to .atlas/rules/*.yaml");
        return Ok(());
    }

    // ── 2. Load data nodes and dataflow edges ───────────────────────────────
    if !json {
        eprintln!("Loading dataflow graph...");
    }

    let all_files = store.list_files().context("Failed to list files")?;

    // Apply file filter
    let file_id_filter: Option<FileId> = file_id_hex.and_then(|h| {
        h.trim().parse().ok()
    });

    let files: Vec<_> = all_files.iter()
        .filter(|f| file_id_filter.as_ref().map_or(true, |fid| f.file_id == *fid))
        .collect();

    let mut all_nodes = Vec::new();
    let mut all_edges = Vec::new();

    for file_info in &files {
        let symbols = store.find_symbols_by_file(&file_info.file_id)
            .context("Failed to load symbols")?;
        for sym in &symbols {
            if let Ok(nodes) = store.find_data_nodes_by_function(&sym.id) {
                all_nodes.extend(nodes);
            }
        }
    }

    for node in &all_nodes {
        let edges = store.find_dataflow_edges_by_source(&node.id)
            .context("Failed to load dataflow edges")?;
        all_edges.extend(edges);
    }

    if !json {
        eprintln!("  Loaded {} data nodes, {} dataflow edges", all_nodes.len(), all_edges.len());
    }

    // ── 3. Run taint engine ─────────────────────────────────────────────────
    if !json {
        eprintln!("Running taint analysis...");
    }
    let engine = TaintEngine::new(rules);
    let result = engine.analyze(&all_nodes, &all_edges);

    if !json {
        eprintln!("  Sources matched: {}", result.sources_matched);
        eprintln!("  Sinks matched:   {}", result.sinks_matched);
        eprintln!("  Paths explored:  {}", result.paths_explored);
    }

    // ── 4. Trace paths ─────────────────────────────────────────────────────
    if !json {
        eprintln!("Tracing taint paths...");
    }
    let tracer = TaintPathTracer::with_max_depth(30);
    let paths = tracer.trace_all(&result.findings, &all_nodes, &all_edges);

    // ── 5. Persist findings and paths ───────────────────────────────────────
    store.insert_taint_findings(&result.findings)
        .context("Failed to persist taint findings")?;

    if !json {
        eprintln!();
        eprintln!("Taint Analysis Results");
        eprintln!("=====================");
        eprintln!("  Total findings: {}", result.findings.len());
    }

    // Group by severity
    let mut by_severity: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for f in &result.findings {
        *by_severity.entry(f.severity.as_str().to_string()).or_default() += 1;
    }

    for (sev, count) in &by_severity {
        eprintln!("    {:<8} {}", sev, count);
    }

    // Print top findings
    let min_sev = severity.and_then(Severity::from_str).unwrap_or(Severity::Low);

    let mut sorted_findings = result.findings.clone();
    sorted_findings.sort_by(|a, b| b.severity.cmp(&a.severity));

    let show_findings: Vec<_> = sorted_findings.iter()
        .filter(|f| f.severity >= min_sev)
        .take(20)
        .collect();

    eprintln!();

    for f in &show_findings {
        let path_steps = paths.iter().find(|tr| tr.finding_id == f.id);
        let step_count = path_steps.map(|p| p.steps.len()).unwrap_or(0);

        eprintln!("  [{}] {} -> {} ({} steps, conf={:.2})",
            f.severity.as_str().to_uppercase(),
            f.source_node.to_hex().chars().take(8).collect::<String>(),
            f.sink_node.to_hex().chars().take(8).collect::<String>(),
            step_count,
            f.confidence.as_f32(),
        );
    }

    // Persist path steps
    let mut all_steps = Vec::new();
    for pt in &paths {
        all_steps.extend(pt.steps.clone());
    }
    store.insert_taint_path_steps(&all_steps)
        .context("Failed to persist taint path steps")?;

    if !json {
        eprintln!();
        eprintln!("  Use `atlas status` to see updated stats.");
        eprintln!("  Use `atlas_taint_findings` MCP tool to query findings.");
    }

    Ok(())
}

fn get_indexed_languages(store: &Store) -> anyhow::Result<Vec<crate::types::enums::Language>> {
    let files = store.list_files().context("Failed to list files")?;
    let langs: std::collections::BTreeSet<_> = files.iter()
        .map(|f| f.language)
        .collect();
    Ok(langs.into_iter().collect())
}
