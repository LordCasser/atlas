//! CLI: clap-based command definitions.

pub mod commands;

use clap::Parser;

/// Atlas -- Local-first semantic knowledge graph builder.
#[derive(Parser, Debug)]
#[command(name = "atlas", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

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
    /// Trace variable dataflow from a source position
    Trace {
        #[arg(short, long, default_value = ".")]
        project: String,
        #[command(subcommand)]
        sub: TraceCmd,
    },
}

#[derive(clap::Subcommand, Debug)]
pub enum TraceCmd {
    /// Resolve a source position to its full context
    Point {
        /// File path relative to project root (e.g. "src/foo.ts")
        #[arg(short, long)]
        file: String,
        /// 1-based line number
        #[arg(short, long)]
        line: u32,
        /// 1-based column number
        #[arg(short, long)]
        column: u32,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Trace a variable's dataflow backward from a source position
    Variable {
        /// File path relative to project root
        #[arg(short, long)]
        file: String,
        /// 1-based line number
        #[arg(short, long)]
        line: u32,
        /// 1-based column number
        #[arg(short, long)]
        column: u32,
        /// Maximum backward traversal depth (default: 30)
        #[arg(long, default_value = "30")]
        max_depth: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Trace how a function gets invoked -- reverse call-graph from target upward
    CallerPath {
        /// Symbol ID in hex (from atlas_search or atlas_symbol)
        #[arg(short, long, required_unless_present = "name")]
        symbol: Option<String>,
        /// Symbol name for lookup (e.g. "inner" instead of hex)
        #[arg(short, long, required_unless_present = "symbol")]
        name: Option<String>,
        /// Maximum backward call depth (default: 20)
        #[arg(long, default_value = "20")]
        max_depth: usize,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
}