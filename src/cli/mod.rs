//! CLI: clap-based command definitions.

pub mod commands;

#[cfg(feature = "cli")]
use clap::Parser;

/// Atlas -- Local-first semantic knowledge graph builder.
#[cfg(feature = "cli")]
#[derive(Parser, Debug)]
#[command(name = "atlas", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[cfg(feature = "cli")]
#[derive(clap::Subcommand, Debug)]
pub enum Commands {
    /// Initialize an Atlas project (creates .atlas/ and database)
    Init {
        /// Project root directory
        #[arg(short, long, default_value = ".")]
        project: String,
    },
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
    },
    /// Incremental sync
    Sync {
        #[arg(short, long, default_value = ".")]
        project: String,
    },
    /// Search for symbols
    Search {
        /// Search query
        query: String,
        #[arg(short, long, default_value = ".")]
        project: String,
        #[arg(short, long, default_value = "10")]
        limit: usize,
    },
    /// Start MCP server
    #[cfg(feature = "mcp")]
    Mcp {
        #[arg(short, long, default_value = ".")]
        project: String,
    },
}
