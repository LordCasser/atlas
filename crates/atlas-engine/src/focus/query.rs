//! QueryIntent — what the MCP tool is asking for.
//!
//! Each variant encodes the required parameters for a specific MCP tool:
//! - `Calls` → `atlas_calls` / `atlas_callers`
//! - `Explore` → `atlas_explore`
//! - `Search` → `atlas_search`
//! - `TracePoint` → `atlas_trace point`
//! - `Context` → `atlas_symbol context`
//! - `TraceVariable` → `atlas_trace variable`

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
    /// `atlas_explore` — investigate a symbol's identity, source, call evidence.
    Explore {
        /// The symbol name to look up.
        symbol_name: String,
        /// Optional FileId if the user specified a specific file.
        file_id: Option<FileId>,
        /// Optional SymbolId if already resolved.
        symbol_id: Option<SymbolId>,
    },
    /// `atlas_search` — search symbols by name within a scope.
    Search {
        /// The search query text.
        query: String,
        /// Optional project-relative directory or file scope.
        scope: Option<String>,
    },
    /// `atlas_symbol context` — structured callers, callees, file peers, imports.
    Context {
        /// The symbol name to look up.
        symbol_name: String,
        /// Optional FileId if the user specified a specific file.
        file_id: Option<FileId>,
        /// Optional SymbolId if already resolved.
        symbol_id: Option<SymbolId>,
    },
    /// `atlas_trace variable` — backward intra-procedural dataflow trace.
    TraceVariable {
        /// The file to trace the variable in.
        file_id: FileId,
        /// 1-based line number.
        line: u32,
        /// 1-based column number.
        column: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::FileId;

    #[test]
    fn test_query_intent_explore_construction() {
        let intent = QueryIntent::Explore {
            symbol_name: "foo".to_string(),
            file_id: None,
            symbol_id: None,
        };
        match intent {
            QueryIntent::Explore { symbol_name, file_id, symbol_id } => {
                assert_eq!(symbol_name, "foo");
                assert!(file_id.is_none());
                assert!(symbol_id.is_none());
            }
            _ => panic!("Expected Explore variant"),
        }
    }

    #[test]
    fn test_query_intent_search_construction() {
        let intent = QueryIntent::Search {
            query: "foo".to_string(),
            scope: None,
        };
        match intent {
            QueryIntent::Search { query, scope } => {
                assert_eq!(query, "foo");
                assert!(scope.is_none());
            }
            _ => panic!("Expected Search variant"),
        }
    }

    #[test]
    fn test_query_intent_context_construction() {
        let intent = QueryIntent::Context {
            symbol_name: "bar".to_string(),
            file_id: None,
            symbol_id: None,
        };
        match intent {
            QueryIntent::Context { symbol_name, file_id, symbol_id } => {
                assert_eq!(symbol_name, "bar");
                assert!(file_id.is_none());
                assert!(symbol_id.is_none());
            }
            _ => panic!("Expected Context variant"),
        }
    }

    #[test]
    fn test_query_intent_trace_variable_construction() {
        let fid = FileId::generate("test.rs");
        let intent = QueryIntent::TraceVariable {
            file_id: fid,
            line: 42,
            column: 10,
        };
        match intent {
            QueryIntent::TraceVariable { file_id, line, column } => {
                assert_eq!(file_id, fid);
                assert_eq!(line, 42);
                assert_eq!(column, 10);
            }
            _ => panic!("Expected TraceVariable variant"),
        }
    }

    #[test]
    fn test_query_intent_explore_debug() {
        let intent = QueryIntent::Explore {
            symbol_name: "foo".to_string(),
            file_id: None,
            symbol_id: None,
        };
        let debug_str = format!("{intent:?}");
        assert!(debug_str.contains("Explore"), "Debug output should contain 'Explore': {debug_str}");
    }

    #[test]
    fn test_query_intent_search_debug() {
        let intent = QueryIntent::Search {
            query: "foo".to_string(),
            scope: None,
        };
        let debug_str = format!("{intent:?}");
        assert!(debug_str.contains("Search"), "Debug output should contain 'Search': {debug_str}");
    }
}
