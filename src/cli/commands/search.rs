//! `atlas search` command — full-text + graph-aware symbol search.

use crate::db::Store;
use crate::search::SearchEngine;
use crate::search::SearchOptions;
use crate::types::SymbolKind;
use anyhow::Context;
use serde::Serialize;
use std::sync::Arc;

pub fn run(
    query: &str,
    project: &str,
    limit: usize,
    kind: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let root = std::path::Path::new(project);
    let store = Store::open(root).context("Failed to open Atlas database")?;
    let store = Arc::new(store);

    // Build graph and search engine
    let graph = crate::graph::GraphEngine::from_store(&store, 0.3)
        .context("Failed to load graph snapshot")?;
    let graph = Arc::new(graph);
    let search = SearchEngine::new(Arc::clone(&store), Arc::clone(&graph));

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

    let results = search.search(query, limit, &options)?;
    if results.is_empty() {
        println!("No results found for '{}'", query);
        return Ok(());
    }

    if json {
        let json_results: Vec<JsonSearchResult> = results
            .iter()
            .map(|r| JsonSearchResult {
                name: r.symbol.name.clone(),
                kind: r.symbol.kind.as_str().to_string(),
                qualified_name: r.symbol.qualified_name.clone(),
                file_path: r.file_path.clone().unwrap_or_default(),
                line: r.symbol.range.start_line,
                signature: r.symbol.signature.clone(),
                score: (r.score.total * 1000.0).round() / 1000.0,
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
            // Human-readable file path + line number
            let path_display = r.file_path.as_deref().unwrap_or("<unknown>");
            let line = r.symbol.range.start_line;
            println!("      file:  {}:{}", path_display, line);
            println!("      qname: {}", r.symbol.qualified_name);
            if let Some(ref sig) = r.symbol.signature {
                println!("      sig:   {}", sig);
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

/// JSON-serializable search result for --json output.
#[derive(Serialize)]
struct JsonSearchResult {
    name: String,
    kind: String,
    qualified_name: String,
    file_path: String,
    line: u32,
    signature: Option<String>,
    score: f64,
}

/// List all valid symbol kind strings for error messages.
fn valid_kinds() -> Vec<&'static str> {
    vec![
        "file", "module", "class", "struct", "interface", "trait",
        "enum", "enum_member", "function", "method", "property",
        "field", "variable", "constant", "type_alias", "namespace",
        "parameter", "constructor", "macro", "decorator", "package",
    ]
}
