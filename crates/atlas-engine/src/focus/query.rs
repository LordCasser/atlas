//! QueryIntent — what the MCP tool is asking for.
//!
//! Each variant encodes the required parameters for a specific MCP tool:
//! - `Calls` → `atlas_calls` / `atlas_callers` / `atlas_callgraph`
//! - `Explore` → `atlas_explore`
//! - `Search` → `atlas_search`
//! - `TracePoint` → `atlas_trace point`
//! - `Context` → `atlas_symbol context`
//! - `TraceVariable` → `atlas_trace variable`
//! - `Path` → `atlas_path`
//! - `Impact` → `atlas_impact`

use types::ids::{FileId, SymbolId};

/// What the MCP tool is asking for.
#[derive(Debug, Clone)]
pub enum QueryIntent {
    /// `atlas_calls` / `atlas_callers` / `atlas_callgraph`
    Calls {
        /// The symbol name to look up.
        symbol_name: String,
        /// Optional FileId if the user specified a specific file.
        file_id: Option<FileId>,
        /// Optional SymbolId if already resolved.
        symbol_id: Option<SymbolId>,
        /// Call graph direction: "incoming", "outgoing", or "both".
        direction: Option<String>,
        /// Call graph traversal depth.
        depth: Option<usize>,
    },
    /// `atlas_path` — shortest path between two symbols
    Path {
        /// The source symbol name.
        from_name: String,
        /// The target symbol name.
        to_name: String,
        /// Optional maximum BFS depth.
        max_depth: Option<usize>,
    },
    /// `atlas_impact` — impact radius analysis
    Impact {
        /// The symbol name to analyze.
        symbol_name: String,
        /// Optional traversal depth (BFS radius).
        depth: Option<usize>,
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
    fn test_query_intent_calls_construction() {
        let intent = QueryIntent::Calls {
            symbol_name: "func".to_string(),
            file_id: None,
            symbol_id: None,
            direction: Some("incoming".to_string()),
            depth: Some(2),
        };
        match intent {
            QueryIntent::Calls { symbol_name, direction, depth, .. } => {
                assert_eq!(symbol_name, "func");
                assert_eq!(direction, Some("incoming".to_string()));
                assert_eq!(depth, Some(2));
            }
            _ => panic!("Expected Calls variant"),
        }
    }

    #[test]
    fn test_query_intent_calls_defaults() {
        let intent = QueryIntent::Calls {
            symbol_name: "func".to_string(),
            file_id: None,
            symbol_id: None,
            direction: None,
            depth: None,
        };
        match intent {
            QueryIntent::Calls { direction, depth, .. } => {
                assert!(direction.is_none());
                assert!(depth.is_none());
            }
            _ => panic!("Expected Calls variant"),
        }
    }

    #[test]
    fn test_query_intent_path_construction() {
        let intent = QueryIntent::Path {
            from_name: "A::foo".to_string(),
            to_name: "B::bar".to_string(),
            max_depth: Some(5),
        };
        match intent {
            QueryIntent::Path { from_name, to_name, max_depth } => {
                assert_eq!(from_name, "A::foo");
                assert_eq!(to_name, "B::bar");
                assert_eq!(max_depth, Some(5));
            }
            _ => panic!("Expected Path variant"),
        }
    }

    #[test]
    fn test_query_intent_path_debug() {
        let intent = QueryIntent::Path {
            from_name: "A::foo".to_string(),
            to_name: "B::bar".to_string(),
            max_depth: None,
        };
        let debug_str = format!("{intent:?}");
        assert!(debug_str.contains("Path"), "Debug output should contain 'Path': {debug_str}");
    }

    #[test]
    fn test_query_intent_impact_construction() {
        let intent = QueryIntent::Impact {
            symbol_name: "main".to_string(),
            depth: Some(3),
        };
        match intent {
            QueryIntent::Impact { symbol_name, depth } => {
                assert_eq!(symbol_name, "main");
                assert_eq!(depth, Some(3));
            }
            _ => panic!("Expected Impact variant"),
        }
    }

    #[test]
    fn test_query_intent_impact_debug() {
        let intent = QueryIntent::Impact {
            symbol_name: "main".to_string(),
            depth: None,
        };
        let debug_str = format!("{intent:?}");
        assert!(debug_str.contains("Impact"), "Debug output should contain 'Impact': {debug_str}");
    }

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
