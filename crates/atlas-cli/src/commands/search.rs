//! `atlas search` command — full-text + graph-aware symbol search, with lazy structural support.

use crate::runtime::{CommandContext, DbMode};
use anyhow::Context;
use atlas_engine::SearchEngine;
use atlas_engine::SearchOptions;
use atlas_engine::{LazyStructuralService, Language, SymbolKind};
use serde::Serialize;
use std::path::PathBuf;
use std::sync::Arc;

pub fn run(
    query: &str,
    project: &str,
    limit: usize,
    kind: Option<&str>,
    language: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let ctx = CommandContext::open(project, DbMode::ExistingReadOnly)?;
    let store_arc = ctx.store;
    let root = &ctx.root;

    // Build graph and search engine
    let graph = atlas_engine::GraphEngine::from_store(&store_arc, 0.3)
        .context("Failed to load graph snapshot")?;
    let graph = Arc::new(graph);
    let search = SearchEngine::new(Arc::clone(&store_arc), Arc::clone(&graph));

    // Build search options from CLI flags
    let mut options = SearchOptions::new();
    if let Some(kind_str) = kind {
        if let Some(kind_val) = SymbolKind::from_str(kind_str) {
            options = options.with_kind(kind_val);
        } else {
            anyhow::bail!(
                "Unknown symbol kind '{}'. Valid kinds: {}",
                kind_str,
                valid_kinds().join(", ")
            );
        }
    }
    if let Some(lang_str) = language {
        match Language::from_str(lang_str) {
            Some(lang) => { options.language = Some(lang); }
            None => anyhow::bail!("Unknown language '{}'", lang_str),
        }
    }

    let results = search.search(query, limit, &options)?;

    // Transparent lazy structural: if no results or only manifest-layer,
    // trigger extraction silently and re-search.
    let needs_lazy = results.is_empty()
        || results.iter().any(|r| r.symbol.layer == "manifest");

    let final_results = if needs_lazy {
        let _ = lazy_structural_for_query(&store_arc, root, query);
        search.search(query, limit, &options)?
    } else {
        results
    };

    if final_results.is_empty() {
        println!("No results found for '{}'", query);
        return Ok(());
    }

    display_results(query, root, &final_results, json)
}

// ── Display ───────────────────────────────────────────────────────────────

fn display_results(
    query: &str,
    root: &std::path::Path,
    results: &[atlas_engine::SearchResult],
    json: bool,
) -> anyhow::Result<()> {
    if json {
        let json_results: Vec<JsonSearchResult> = results
            .iter()
            .map(|r| {
                let snippet = read_source_snippet(root, &r.file_path, r.symbol.range.start_line);
                JsonSearchResult {
                    name: r.symbol.name.clone(),
                    kind: r.symbol.kind.as_str().to_string(),
                    qualified_name: r.symbol.qualified_name.clone(),
                    file_path: r.file_path.clone().unwrap_or_default(),
                    line: r.symbol.range.start_line + 1,
                    signature: r.symbol.signature.clone(),
                    snippet,
                    score: (r.score.total * 1000.0).round() / 1000.0,
                }
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&json_results)?);
    } else {
        println!("Search results for '{}':", query);
        println!("{:-<80}", "");
        for (i, r) in results.iter().enumerate() {
            println!(
                "{:>3}. {:<30} [{:<12}] score={:.3}",
                i + 1, &r.symbol.name, r.symbol.kind.as_str(), r.score.total,
            );
            let path_display = r.file_path.as_deref().unwrap_or("<unknown>");
            let line = r.symbol.range.start_line + 1;
            println!("      file:  {}:{}", path_display, line);
            println!("      qname: {}", r.symbol.qualified_name);
            println!("      layer: {}", r.symbol.layer);
            if let Some(ref sig) = r.symbol.signature {
                println!("      sig:   {}", sig);
            }
            if let Some(snippet) = read_source_snippet(root, &r.file_path, line) {
                println!("      code:  {}", snippet.trim());
            }
            if !r.matched_field.is_empty() {
                println!("      field: {}", r.matched_field);
            }
            println!();
        }
        println!("{} results shown.", results.len());
    }
    Ok(())
}

// ── Lazy structural trigger ───────────────────────────────────────────────

fn lazy_structural_for_query(
    store: &Arc<atlas_engine::Store>,
    root: &PathBuf,
    query: &str,
) -> anyhow::Result<()> {
    let lazy = LazyStructuralService::new(Arc::clone(store), Some(root.clone()));
    let result = lazy.ensure_structural_for_symbol(query)?;
    if result.files_built > 0 {
        tracing::info!("Lazy structural: extracted {} files for '{}'", result.files_built, query);
    }
    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────

fn read_source_snippet(
    project_root: &std::path::Path,
    file_path: &Option<String>,
    line_num: u32,
) -> Option<String> {
    let path_str = file_path.as_ref()?;
    let full_path = project_root.join(path_str);
    let canonical = full_path.canonicalize().ok()?;
    let canonical_root = project_root.canonicalize().ok()?;
    if !canonical.starts_with(&canonical_root) {
        return None;
    }
    let content = std::fs::read_to_string(&canonical).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let idx = (line_num as usize).saturating_sub(1);
    if idx >= lines.len() { return None; }
    let end = (idx + 2).min(lines.len());
    Some(lines[idx..end].join("\n       "))
}

#[derive(Serialize)]
struct JsonSearchResult {
    name: String,
    kind: String,
    qualified_name: String,
    file_path: String,
    line: u32,
    signature: Option<String>,
    snippet: Option<String>,
    score: f64,
}

fn valid_kinds() -> Vec<&'static str> {
    vec![
        "file", "module", "class", "struct", "interface", "trait", "enum",
        "enum_member", "function", "method", "property", "field", "variable",
        "constant", "type_alias", "namespace", "parameter", "constructor",
        "macro", "decorator", "package",
    ]
}
