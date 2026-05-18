//! `atlas mcp` command — start the MCP (Model Context Protocol) server.
//!
//! Reads JSON-RPC requests from stdin, writes responses to stdout,
//! using Content-Length header framing.

#![cfg(feature = "mcp")]

use anyhow::Context;
use std::sync::Arc;

use crate::db::Store;

pub fn run(project: &str) -> anyhow::Result<()> {
    let _ = project; // project root is auto-detected
    let root = Store::find_project_root()
        .context("No .atlas directory found (run 'atlas init' first)")?;
    let db_path = root.join(".atlas").join("atlas.db");
    let store = crate::db::Store::open(&db_path)?;
    store.init_schema()?;

    let store = Arc::new(store);
    tracing::info!("Starting Atlas MCP server...");
    tracing::info!("Project: {}", root.display());

    let server = crate::mcp::McpServer::new(Arc::clone(&store));
    server.serve()
}
