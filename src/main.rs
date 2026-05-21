use atlas::cli::{Cli, Commands, TraceCmd};
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
                    &query,
                    &project,
                    limit,
                    kind.as_deref(),
                    language.as_deref(),
                    json,
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
            Commands::Trace { project, sub } => match sub {
                TraceCmd::Point {
                    file,
                    line,
                    column,
                    json,
                } => {
                    atlas::cli::commands::trace::run_point(&project, &file, line, column, json)?;
                }
                TraceCmd::Variable {
                    file,
                    line,
                    column,
                    max_depth,
                    json,
                } => {
                    atlas::cli::commands::trace::run_variable(
                        &project, &file, line, column, max_depth, json,
                    )?;
                }
                TraceCmd::CallerPath {
                    symbol,
                    name,
                    max_depth,
                    json,
                } => {
                    atlas::cli::commands::trace::run_caller_path(
                        &project,
                        symbol.as_deref(),
                        name.as_deref(),
                        max_depth,
                        json,
                    )?;
                }
            },
        }
    }

    #[cfg(not(feature = "cli"))]
    {
        eprintln!("Atlas compiled without CLI support. Enable the 'cli' feature.");
    }

    Ok(())
}
