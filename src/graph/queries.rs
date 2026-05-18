//! High-level graph queries: call graph, type hierarchy, impact radius.

use crate::types::SymbolDef;

pub struct CallGraph {
    pub callers: Vec<SymbolDef>,
    pub callees: Vec<SymbolDef>,
}

pub struct TypeHierarchy {
    pub ancestors: Vec<SymbolDef>,
    pub descendants: Vec<SymbolDef>,
}

// TODO: M4 — implement high-level graph queries
