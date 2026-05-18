//! Framework-specific resolvers.

pub mod react;

use crate::types::{EdgeKind, ReferenceUse, SymbolId};

/// Trait for framework-specific reference resolution.
pub trait FrameworkResolver: Send + Sync {
    fn framework_name(&self) -> &str;
    fn resolve(&self, _unres: &ReferenceUse, _ctx: &ResolutionContext) -> Option<(SymbolId, f64)> {
        None
    }
    fn supported_edge_kinds(&self) -> &[EdgeKind];
}

/// Context available during resolution.
pub struct ResolutionContext {
    // TODO: M5 — populate with relevant context
}
