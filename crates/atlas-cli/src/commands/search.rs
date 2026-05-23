//! `atlas search` command — full-text + graph-aware symbol search.

use crate::runtime::{CommandContext, DbMode};
use anyhow::Context;
use atlas_engine::SearchEngine;
use atlas_engine::SearchOptions;
use atlas_engine::{Language, SymbolKind};
use serde::Serialize;
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
            Some(lang) => {
                options.language = Some(lang);
            }
            None => {
                anyhow::bail!("Unknown language '{}'", lang_str);
            }
        }
    }

    let results = search.search(query, limit, &options)?;
    if results.is_empty() {
        println!("No results found for '{}'", query);
        return Ok(());
    }

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
                    line: r.symbol.range.start_line + 1, // 1-indexed for editor compatibility
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
                i + 1,
                &r.symbol.name,
                r.symbol.kind.as_str(),
                r.score.total,
            );
            // Human-readable file path + line number (1-indexed for editor compatibility)
            let path_display = r.file_path.as_deref().unwrap_or("<unknown>");
            let line = r.symbol.range.start_line + 1; // tree-sitter rows are 0-indexed
            println!("      file:  {}:{}", path_display, line);
            println!("      qname: {}", r.symbol.qualified_name);
            if let Some(ref sig) = r.symbol.signature {
                println!("      sig:   {}", sig);
            }
            // Source code snippet (the definition line + next line for context)
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

/// Read a source line from the file for snippet display.
/// Returns the line at `line_num` (1-indexed) with at most 1 trailing line for context.
fn read_source_snippet(
    project_root: &std::path::Path,
    file_path: &Option<String>,
    line_num: u32,
) -> Option<String> {
    let path_str = file_path.as_ref()?;
    let full_path = project_root.join(path_str);
    let canonical = full_path.canonicalize().ok()?;
    // Canonicalize root too — macOS /var→/private/var symlink
    let canonical_root = project_root.canonicalize().ok()?;
    if !canonical.starts_with(&canonical_root) {
        return None;
    }
    let content = std::fs::read_to_string(&canonical).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let idx = (line_num as usize).saturating_sub(1);
    if idx >= lines.len() {
        return None;
    }
    // Return the line at the symbol, plus the next line if available
    let end = (idx + 2).min(lines.len());
    Some(lines[idx..end].join("\n       "))
}

/// JSON-serializable search result for --json output.
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

/// List all valid symbol kind strings for error messages.
fn valid_kinds() -> Vec<&'static str> {
    vec![
        "file",
        "module",
        "class",
        "struct",
        "interface",
        "trait",
        "enum",
        "enum_member",
        "function",
        "method",
        "property",
        "field",
        "variable",
        "constant",
        "type_alias",
        "namespace",
        "parameter",
        "constructor",
        "macro",
        "decorator",
        "package",
    ]
}
