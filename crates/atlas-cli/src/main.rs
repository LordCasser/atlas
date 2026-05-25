use atlas_cli::logging::LogFormat;
use atlas_cli::{Cli, Commands, LogFormatArg, TraceCmd};
use clap::Parser;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Determine MCP mode before initialising logging (MCP defaults to info).
    #[cfg(feature = "mcp")]
    let is_mcp = matches!(cli.command, Commands::Mcp { .. });
    #[cfg(not(feature = "mcp"))]
    let is_mcp = false;

    // Initialise structured tracing → stderr.  This must happen BEFORE any
    // command dispatch so that spans and events are captured.
    let verbosity = cli.verbosity(is_mcp);
    let format = match cli.log_format() {
        LogFormatArg::Compact => LogFormat::Compact,
        LogFormatArg::Json => LogFormat::Json,
    };
    atlas_cli::logging::init(verbosity, format);

    // Dispatch with a root span that tags the command name and project path.
    match &cli.command {
        Commands::Init { project } => {
            let _span = tracing::info_span!("init", project = %project).entered();
            tracing::info!("atlas init starting");
            atlas_cli::commands::init::run(project)?;
            tracing::info!("atlas init complete");
        }
        Commands::Status { project } => {
            let _span = tracing::info_span!("status", project = %project).entered();
            atlas_cli::commands::status::run(project)?;
        }
        Commands::Doctor { project } => {
            let _span = tracing::info_span!("doctor", project = %project).entered();
            atlas_cli::commands::doctor::run(project)?;
        }
        Commands::Index {
            project,
            include,
            exclude,
            analysis,
        } => {
            let _span = tracing::info_span!("index", project = %project).entered();
            atlas_cli::commands::index::run(project, include.as_deref(), exclude.as_deref(), analysis)?;
        }
        Commands::Sync { project } => {
            let _span = tracing::info_span!("sync", project = %project).entered();
            atlas_cli::commands::sync::run(project)?;
        }
        Commands::Search {
            query,
            project,
            limit,
            kind,
            language,
            json,
        } => {
            let _span = tracing::info_span!("search", project = %project, query = %query).entered();
            atlas_cli::commands::search::run(
                query,
                project,
                *limit,
                kind.as_deref(),
                language.as_deref(),
                *json,
            )?;
        }
        Commands::Context { query, project } => {
            let _span =
                tracing::info_span!("context", project = %project, query = %query).entered();
            atlas_cli::commands::context::run(query, project)?;
        }
        Commands::Files { project } => {
            let _span = tracing::info_span!("files", project = %project).entered();
            atlas_cli::commands::files::run(project)?;
        }
        #[cfg(feature = "mcp")]
        Commands::Mcp { project } => {
            let _span = tracing::info_span!("mcp", project = %project).entered();
            atlas_cli::commands::mcp::run(project)?;
        }
        Commands::Trace { project, sub } => {
            let _span = tracing::info_span!("trace", project = %project).entered();
            match sub {
                TraceCmd::Point {
                    file,
                    line,
                    column,
                    json,
                } => {
                    atlas_cli::commands::trace::run_point(project, file, *line, *column, *json)?;
                }
                TraceCmd::Variable {
                    file,
                    line,
                    column,
                    max_depth,
                    json,
                } => {
                    atlas_cli::commands::trace::run_variable(
                        project, file, *line, *column, *max_depth, *json,
                    )?;
                }
                TraceCmd::CallerPath {
                    symbol,
                    name,
                    max_depth,
                    json,
                } => {
                    atlas_cli::commands::trace::run_caller_path(
                        project,
                        symbol.as_deref(),
                        name.as_deref(),
                        *max_depth,
                        *json,
                    )?;
                }
            }
        }
    }

    Ok(())
}
