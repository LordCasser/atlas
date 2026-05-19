//! `atlas context` command — build AI context around a symbol or query.

use crate::context::ContextBuilder;
use crate::db::Store;
use crate::graph::GraphEngine;
use anyhow::Context;
use std::sync::Arc;

pub fn run(query: &str, project: &str) -> anyhow::Result<()> {
    let root = std::path::Path::new(project);
    let store = Store::open(root).context("Failed to open Atlas database")?;
    let store = Arc::new(store);

    let graph = GraphEngine::from_store(&store, 0.3)
        .context("Failed to load graph snapshot")?;
    let graph = Arc::new(graph);

    let builder = ContextBuilder::new(Arc::clone(&store), Arc::clone(&graph));

    match builder.build_context_for_query(query)? {
        Some(view) => {
            println!("{}", view.to_markdown());
        }
        None => {
            println!("No symbol found for '{}'. Try a different query.", query);
        }
    }

    Ok(())
}
