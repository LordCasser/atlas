use atlas::cli::{Cli, Commands};
use clap::Parser;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    #[cfg(feature = "cli")]
    {
        let cli = Cli::parse();
        match cli.command {
            Commands::Index { project } => {
                println!("Indexing project: {}", project);
                // TODO: Phase 13
            }
            Commands::Sync { project } => {
                println!("Syncing project: {}", project);
                // TODO: Phase 13
            }
            Commands::Search { query, project, limit } => {
                println!("Searching '{}' in {} (limit: {})", query, project, limit);
                // TODO: Phase 13
            }
            Commands::Mcp { project } => {
                println!("Starting MCP server for: {}", project);
                // TODO: Phase 13
            }
        }
    }

    #[cfg(not(feature = "cli"))]
    {
        eprintln!("Atlas compiled without CLI support. Enable the 'cli' feature.");
    }

    Ok(())
}
