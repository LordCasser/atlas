//! `atlas mcp` command — start the MCP (Model Context Protocol) server.
//!
//! Reads JSON-RPC requests from stdin, writes responses to stdout,
//! using Content-Length header framing.

#![cfg(feature = "mcp")]

use anyhow::Context;
use std::path::Path;
use std::sync::Arc;

use atlas_db::Store;
use atlas_workspace::Workspace;

pub fn run(project: &str) -> anyhow::Result<()> {
    let ws = if project == "." {
        Workspace::find().context("No .atlas directory found (run 'atlas init' first)")?
    } else {
        Workspace::open(Path::new(project))
            .with_context(|| format!("Invalid project path: {}", project))?
    };
    if !ws.db_path().is_file() {
        anyhow::bail!("Not an initialized Atlas project. Run `atlas init` first.");
    }
    let store = Store::open_db(ws.db_path())?;

    let store = Arc::new(store);
    tracing::info!("Starting Atlas MCP server...");
    tracing::info!("Project: {}", ws.root().display());

    let server = atlas_mcp::McpServer::new(Arc::clone(&store), ws);
    server.serve()
}
