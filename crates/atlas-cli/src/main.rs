use atlas_cli::{Cli, Commands};
use clap::Parser;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    // Determine MCP mode before initialising logging (MCP defaults to info).
    #[cfg(feature = "mcp")]
    let is_mcp = matches!(cli.command, Some(Commands::Mcp { .. }));
    #[cfg(not(feature = "mcp"))]
    let is_mcp = false;

    // Initialise structured tracing → stderr.  This must happen BEFORE any
    // command dispatch so that spans and events are captured.
    let verbosity = cli.verbosity(is_mcp);
    let format = cli.log_format();
    atlas_cli::logging::init(verbosity, format);

    // No subcommand → launch TUI immediately.
    // Auto-index runs in background if DB is empty; the TUI starts right away.
    let command = match cli.command {
        None => {
            let project_root = PathBuf::from(".");
            return atlas_cli::tui::run_tui(project_root);
        }
        Some(cmd) => cmd,
    };

    // Dispatch with a root span that tags the command name and project path.
    match command {
        Commands::Status { project } => {
            let _span = tracing::info_span!("status", project = %project).entered();
            atlas_cli::commands::status::run(&project)?;
        }
        Commands::Doctor { project } => {
            let _span = tracing::info_span!("doctor", project = %project).entered();
            atlas_cli::commands::doctor::run(&project)?;
        }
        Commands::Index {
            project,
            include,
            scope,
            exclude,
            analysis,
            force_reindex,
        } => {
            let _span = tracing::info_span!("index", project = %project).entered();
            atlas_cli::commands::index::run_with_options(
                &project,
                &include,
                &scope,
                &exclude,
                &analysis,
                force_reindex,
            )?;
        }
        Commands::Sync {
            project,
            analysis,
            force_reindex,
        } => {
            let _span = tracing::info_span!("sync", project = %project).entered();
            atlas_cli::commands::sync::run_with_options(&project, &analysis, force_reindex)?;
        }
        Commands::Files { project } => {
            let _span = tracing::info_span!("files", project = %project).entered();
            atlas_cli::commands::files::run(&project)?;
        }
        #[cfg(feature = "mcp")]
        Commands::Mcp { project } => {
            let _span = tracing::info_span!("mcp", project = %project).entered();
            atlas_cli::commands::mcp::run(&project)?;
        }
    }

    Ok(())
}
