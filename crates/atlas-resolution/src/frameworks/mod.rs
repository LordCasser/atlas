//! Framework-specific resolvers.

pub mod react;

use atlas_types::{EdgeKind, ReferenceUse, SymbolId};

use super::context::ResolutionContext;

/// Trait for framework-specific reference resolution.
pub trait FrameworkResolver: Send + Sync {
    fn framework_name(&self) -> &str;
    fn resolve(&self, _unres: &ReferenceUse, _ctx: &ResolutionContext) -> Option<(SymbolId, f64)> {
        None
    }
    fn supported_edge_kinds(&self) -> &[EdgeKind];
}
