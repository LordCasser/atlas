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
            Commands::Index {
                project,
                include,
                exclude,
            } => {
                atlas::cli::commands::index::run(&project, include.as_deref(), exclude.as_deref())?;
            }
            Commands::Sync { project } => {
                atlas::cli::commands::sync::run(&project)?;
            }
            Commands::Search {
                query,
                project,
                limit,
                kind,
                language,
                json,
            } => {
                atlas::cli::commands::search::run(
                    &query, &project, limit, kind.as_deref(), language.as_deref(), json,
                )?;
            }
            Commands::Context { query, project } => {
                atlas::cli::commands::context::run(&query, &project)?;
            }
            Commands::Files { project } => {
                atlas::cli::commands::files::run(&project)?;
            }
            #[cfg(feature = "mcp")]
            Commands::Mcp { project } => {
                atlas::cli::commands::mcp::run(&project)?;
            }
            Commands::Taint {
                project,
                file_id,
                severity,
                json,
            } => {
                atlas::cli::commands::taint::run(
                    &project, file_id.as_deref(), severity.as_deref(), json,
                )?;
            }
        }
    }

    #[cfg(not(feature = "cli"))]
    {
        eprintln!("Atlas compiled without CLI support. Enable the 'cli' feature.");
    }

    Ok(())
}
