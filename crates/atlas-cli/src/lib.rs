//! CLI: clap-based command definitions.

pub mod commands;
pub mod logging;
pub mod runtime;
pub mod tui;

use clap::Parser;

/// Log output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormatArg {
    Compact,
    Json,
}

/// Atlas -- Local-first semantic knowledge graph builder.
#[derive(Parser, Debug)]
#[command(name = "atlas", version, about)]
pub struct Cli {
    /// Increase diagnostic output (info level). Stderr only.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Enable debug-level diagnostics. Stderr only.
    #[arg(long, global = true)]
    pub debug: bool,

    /// Log format: "json" for structured JSON, default is compact.
    #[arg(long, global = true, default_value = "compact", value_parser = ["compact", "json"])]
    pub log_format: String,

    #[command(subcommand)]
    pub command: Option<Commands>,
}

impl Cli {
    /// Derive the effective verbosity from CLI flags.
    ///
    /// `is_mcp` signals that the command is an MCP server; MCP always defaults
    /// to `info` level so that operational events are never silent.
    pub fn verbosity(&self, is_mcp: bool) -> logging::Verbosity {
        if is_mcp {
            // MCP: verbose (info) floor, debug if --debug is set
            if self.debug {
                return logging::Verbosity::Debug;
            }
            return logging::Verbosity::Verbose;
        }
        if self.debug {
            logging::Verbosity::Debug
        } else if self.verbose {
            logging::Verbosity::Verbose
        } else {
            logging::Verbosity::Default
        }
    }

    /// Derive the effective log format.
    pub fn log_format(&self) -> LogFormatArg {
        match self.log_format.as_str() {
            "json" => LogFormatArg::Json,
            _ => LogFormatArg::Compact,
        }
    }
}

#[derive(clap::Subcommand, Debug)]
pub enum Commands {
    /// Show project indexing status and statistics
    Status {
        /// Project root directory
        #[arg(short, long, default_value = ".")]
        project: String,
    },
    /// Check environment readiness (SQLite FTS5, grammar support, schema version)
    Doctor {
        /// Project root directory
        #[arg(short, long, default_value = ".")]
        project: String,
    },
    /// Index a codebase
    Index {
        /// Project root directory
        #[arg(short, long, default_value = ".")]
        project: String,
        /// Only include files matching these glob patterns (can be specified multiple times, e.g. --include "src/**")
        #[arg(long)]
        include: Vec<String>,
        /// Restrict indexing to these directories (convenience: --scope drivers/net is equivalent to --include "drivers/net/**")
        #[arg(long)]
        scope: Vec<String>,
        /// Exclude files matching these glob patterns (can be specified multiple times, e.g. "**/*.test.ts")
        #[arg(long)]
        exclude: Vec<String>,
        /// Analysis depth: "manifest" (fastest, top-level symbols only), "structural" (default, symbols+references+callgraph), or "full" (slower, complete dataflow/CFG)
        #[arg(long, default_value = "structural", value_parser = ["manifest", "structural", "full"])]
        analysis: String,
        /// Allow a lower analysis depth to replace an existing structural/full index.
        #[arg(long)]
        force_reindex: bool,
    },
    /// Incremental sync
    Sync {
        #[arg(short, long, default_value = ".")]
        project: String,
        /// Analysis depth: "structural" (default) | "manifest" (top-level only) | "full"
        #[arg(long, default_value = "structural", value_parser = ["manifest", "structural", "full"])]
        analysis: String,
        /// Allow a lower analysis depth to replace an existing structural/full index.
        #[arg(long)]
        force_reindex: bool,
    },
    /// List indexed files
    Files {
        #[arg(short, long, default_value = ".")]
        project: String,
    },
    /// Start MCP server
    #[cfg(feature = "mcp")]
    Mcp {
        #[arg(short, long, default_value = ".")]
        project: String,
    },
    /// Manage focus analysis state
    Focus {
        #[command(subcommand)]
        command: commands::focus::FocusCommand,
    },
}
