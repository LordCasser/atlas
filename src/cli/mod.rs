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
        /// Only include files matching this glob pattern (e.g. "src/**/*.rs")
        #[arg(long)]
        include: Option<String>,
        /// Exclude files matching this glob pattern (e.g. "**/*.test.ts")
        #[arg(long)]
        exclude: Option<String>,
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
        /// Filter by symbol kind (e.g. class, function, method, variable)
        #[arg(short, long)]
        kind: Option<String>,
        /// Filter by language (e.g. python, typescript, java)
        #[arg(short = 'L', long)]
        language: Option<String>,
        /// Output results as JSON
        #[arg(long)]
        json: bool,
    },
    /// Build AI context around a symbol (callers, callees, peers)
    Context {
        /// Symbol name or search query
        query: String,
        #[arg(short, long, default_value = ".")]
        project: String,
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
}
