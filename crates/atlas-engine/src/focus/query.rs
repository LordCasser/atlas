//! QueryIntent — what the MCP tool is asking for.
//!
//! Each variant encodes the required parameters for a specific MCP tool:
//! - `Calls` → `atlas_calls` / `atlas_callers` / `atlas_explore`
//! - `TracePoint` → `atlas_trace point`
//!
//! Future intents (Search, Context, TraceVariable) are planned for later phases.

use types::ids::{FileId, SymbolId};

/// What the MCP tool is asking for.
#[derive(Debug, Clone)]
pub enum QueryIntent {
    /// `atlas_calls` / `atlas_callers` / `atlas_explore`
    Calls {
        /// The symbol name to look up.
        symbol_name: String,
        /// Optional FileId if the user specified a specific file.
        file_id: Option<FileId>,
        /// Optional SymbolId if already resolved.
        symbol_id: Option<SymbolId>,
    },
    /// `atlas_trace point`
    TracePoint {
        /// The file to resolve the position in.
        file_id: FileId,
        /// 1-based line number.
        line: u32,
        /// 1-based column number.
        column: u32,
    },
    // Future intents:
    // Search { query: String, scope: Option<String> },
    // Context { symbol_id: SymbolId },
    // TraceVariable { file_id: FileId, line: u32, column: u32 },
}
