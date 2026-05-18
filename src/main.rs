use atlas::cli::{Cli, Commands};
use clap::Parser;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    #[cfg(feature = "cli")]
    {
        let cli = Cli::parse();
        match cli.command {
            Commands::Init { project } => {
                atlas::cli::commands::init::run(&project)?;
            }
            Commands::Status { project } => {
                atlas::cli::commands::status::run(&project)?;
            }
            Commands::Doctor { project } => {
                atlas::cli::commands::doctor::run(&project)?;
            }
            Commands::Index { project } => {
                atlas::cli::commands::index::run(&project)?;
            }
            Commands::Sync { project } => {
                atlas::cli::commands::sync::run(&project)?;
            }
            Commands::Search {
                query,
                project,
                limit,
            } => {
                atlas::cli::commands::search::run(&query, &project, limit)?;
            }
            #[cfg(feature = "mcp")]
            Commands::Mcp { project } => {
                atlas::cli::commands::mcp::run(&project)?;
            }
        }
    }

    #[cfg(not(feature = "cli"))]
    {
        eprintln!("Atlas compiled without CLI support. Enable the 'cli' feature.");
    }

    Ok(())
}
