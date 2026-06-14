//! `atlas mcp` command — start the MCP (Model Context Protocol) server.
//!
//! Reads JSON-RPC requests from stdin, writes responses to stdout,
//! using rmcp's newline-delimited JSON-RPC stdio transport.

pub fn run(project: &str) -> anyhow::Result<()> {
    tracing::info!("Starting Atlas MCP server...");
    tracing::info!(
        "Initial project argument ignored by MCP startup; call project(action=\"open\", project_path=\"{}\") from the MCP client.",
        project
    );

    let server = atlas_mcp::McpServer::new_unopened();
    server.serve()
}
