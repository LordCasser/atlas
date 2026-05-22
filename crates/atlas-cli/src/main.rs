use atlas_cli::{Cli, Commands, TraceCmd};
use clap::Parser;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Init { project } => {
            atlas_cli::commands::init::run(&project)?;
        }
        Commands::Status { project } => {
            atlas_cli::commands::status::run(&project)?;
        }
        Commands::Doctor { project } => {
            atlas_cli::commands::doctor::run(&project)?;
        }
        Commands::Index {
            project,
            include,
            exclude,
        } => {
            atlas_cli::commands::index::run(&project, include.as_deref(), exclude.as_deref())?;
        }
        Commands::Sync { project } => {
            atlas_cli::commands::sync::run(&project)?;
        }
        Commands::Search {
            query,
            project,
            limit,
            kind,
            language,
            json,
        } => {
            atlas_cli::commands::search::run(
                &query,
                &project,
                limit,
                kind.as_deref(),
                language.as_deref(),
                json,
            )?;
        }
        Commands::Context { query, project } => {
            atlas_cli::commands::context::run(&query, &project)?;
        }
        Commands::Files { project } => {
            atlas_cli::commands::files::run(&project)?;
        }
        #[cfg(feature = "mcp")]
        Commands::Mcp { project } => {
            atlas_cli::commands::mcp::run(&project)?;
        }
        Commands::Trace { project, sub } => match sub {
            TraceCmd::Point {
                file,
                line,
                column,
                json,
            } => {
                atlas_cli::commands::trace::run_point(&project, &file, line, column, json)?;
            }
            TraceCmd::Variable {
                file,
                line,
                column,
                max_depth,
                json,
            } => {
                atlas_cli::commands::trace::run_variable(
                    &project, &file, line, column, max_depth, json,
                )?;
            }
            TraceCmd::CallerPath {
                symbol,
                name,
                max_depth,
                json,
            } => {
                atlas_cli::commands::trace::run_caller_path(
                    &project,
                    symbol.as_deref(),
                    name.as_deref(),
                    max_depth,
                    json,
                )?;
            }
        },
    }

    Ok(())
}
