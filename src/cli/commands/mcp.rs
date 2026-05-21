//! `atlas mcp` command — start the MCP (Model Context Protocol) server.
//!
//! Reads JSON-RPC requests from stdin, writes responses to stdout,
//! using Content-Length header framing.

#![cfg(feature = "mcp")]

use anyhow::Context;
use std::sync::Arc;

use crate::db::Store;

pub fn run(project: &str) -> anyhow::Result<()> {
    let root = if project == "." {
        Store::find_project_root().context("No .atlas directory found (run 'atlas init' first)")?
    } else {
        std::path::PathBuf::from(project)
    };
    let store = Store::open(&root)?;
    store.init_schema()?;

    let store = Arc::new(store);
    tracing::info!("Starting Atlas MCP server...");
    tracing::info!("Project: {}", root.display());

    let server = crate::mcp::McpServer::new(Arc::clone(&store), root);
    server.serve()
}
