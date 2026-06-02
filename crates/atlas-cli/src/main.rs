use anyhow::Context;
use atlas_cli::logging::LogFormat;
use atlas_cli::{Cli, Commands, LogFormatArg};
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
    let format = match cli.log_format() {
        LogFormatArg::Compact => LogFormat::Compact,
        LogFormatArg::Json => LogFormat::Json,
    };
    atlas_cli::logging::init(verbosity, format);

    // No subcommand → launch TUI.
    let command = match cli.command {
        None => {
            let project_root = PathBuf::from(".");
            ensure_index_before_tui(&project_root)?;
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
        } => {
            let _span = tracing::info_span!("index", project = %project).entered();
            atlas_cli::commands::index::run(&project, &include, &scope, &exclude, &analysis)?;
        }
        Commands::Sync { project, analysis } => {
            let _span = tracing::info_span!("sync", project = %project).entered();
            atlas_cli::commands::sync::run(&project, &analysis)?;
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

fn ensure_index_before_tui(project_root: &std::path::Path) -> anyhow::Result<()> {
    let db_path = project_root.join(".atlas").join("atlas.db");
    let needs_index = if !db_path.is_file() {
        true
    } else {
        let store = atlas_engine::Store::open_db(&db_path)
            .with_context(|| format!("Failed to open database at {}", db_path.display()))?;
        store.get_stats().map(|stats| stats.total_files == 0)?
    };

    if needs_index {
        let empty: Vec<String> = Vec::new();
        atlas_cli::commands::index::run(".", &empty, &empty, &empty, "structural")?;
    }

    Ok(())
}
