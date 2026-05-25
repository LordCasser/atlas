//! `atlas mcp` command — start the MCP (Model Context Protocol) server.
//!
//! Reads JSON-RPC requests from stdin, writes responses to stdout,
//! using Content-Length header framing.

#![cfg(feature = "mcp")]

use crate::runtime::{CommandContext, DbMode};

pub fn run(project: &str) -> anyhow::Result<()> {
    // Use CreateOrOpenReadWrite so the MCP server auto-initialises
    // the database and schema when pointed at a fresh project.
    // Without this, users would need to run `atlas init` or
    // `atlas index` before starting the MCP server.
    let ctx = CommandContext::find_and_open(project, DbMode::CreateOrOpenReadWrite)?;

    tracing::info!("Starting Atlas MCP server...");
    tracing::info!("Project: {}", ctx.workspace.root().display());

    let server = atlas_mcp::McpServer::new(ctx.store, ctx.workspace);
    server.serve()
}
