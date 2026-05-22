//! `atlas mcp` command — start the MCP (Model Context Protocol) server.
//!
//! Reads JSON-RPC requests from stdin, writes responses to stdout,
//! using Content-Length header framing.

#![cfg(feature = "mcp")]

use crate::runtime::{CommandContext, DbMode};

pub fn run(project: &str) -> anyhow::Result<()> {
    let ctx = CommandContext::find_and_open(project, DbMode::ExistingReadOnly)?;

    tracing::info!("Starting Atlas MCP server...");
    tracing::info!("Project: {}", ctx.workspace.root().display());

    let server = atlas_mcp::McpServer::new(ctx.store, ctx.workspace);
    server.serve()
}
