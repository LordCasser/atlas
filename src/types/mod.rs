//! Atlas core type system: IDs, enums, and the Intermediate Representation (IR).
//!
//! ## Layering
//! - `ids`  — 7 typed blake3 newtypes stored as BLOB in SQLite.
//! - `enums` — 11 enum families describing language, kind, visibility, etc.
//! - `structs` — the core IR: SymbolDef, ReferenceUse, FileFacts, etc.
//!
//! ## Invariants
//! - IDs are deterministically derived (same inputs → same [u8; 32]).
//! - References are preserved after resolution (via `resolved: Option<...>`).
//! - All semantic edges carry `Confidence` and `Provenance`.

mod ids;
mod enums;
mod structs;

// --- IDs ---
pub use ids::{CallsiteId, EdgeId, FileId, ImportId, ReferenceId, ScopeId, SymbolId};

// --- Enums ---
pub use enums::{
    EdgeKind, ImportKind, Language, ParseStatus, Provenance, ReferenceKind, ResolutionStatus,
    ResolutionStrategy, ScopeKind, SymbolKind, Visibility,
};
pub use enums::Confidence;

// --- Core IR ---
pub use structs::{
    ArgumentFact, Callsite, DiagnosticLevel, ExtractDiagnostic, FileFacts, FileInfo, ImportDef,
    RawEdge, ReferenceUse, ResolvedTarget, ScopeDef, SymbolDef, TextRange,
};
