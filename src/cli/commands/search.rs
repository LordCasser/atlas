//! `atlas search` command — full-text + graph-aware symbol search.

use crate::db::Store;
use crate::search::SearchEngine;
use anyhow::Context;
use std::sync::Arc;

pub fn run(query: &str, project: &str, limit: usize) -> anyhow::Result<()> {
    let root = std::path::Path::new(project);
    let store = Store::open(root).context("Failed to open Atlas database")?;
    let store = Arc::new(store);

    // Build graph and search engine
    let graph = crate::graph::GraphEngine::from_store(&store, 0.3)
        .context("Failed to load graph snapshot")?;
    let graph = Arc::new(graph);
    let search = SearchEngine::new(Arc::clone(&store), Arc::clone(&graph));

    let results = search.search(query, limit)?;
    if results.is_empty() {
        println!("No results found for '{}'", query);
        return Ok(());
    }

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
        println!("      qname: {}", r.symbol.qualified_name);
        println!("      file:  {}", r.symbol.file_id);
        if !r.matched_field.is_empty() {
            println!("      field: {}", r.matched_field);
        }
        println!();
    }

    println!("{} results shown.", results.len());

    Ok(())
}
