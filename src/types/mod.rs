//! Atlas core type system: IDs, enums, and the Intermediate Representation (IR).
//!
//! ## Layering
//! - `ids`  — 11 typed blake3 newtypes stored as BLOB in SQLite.
//! - `enums` — 14 enum families describing language, kind, visibility, etc.
//! - `structs` — the core IR: SymbolDef, ReferenceUse, FileFacts, etc.
//! - `bindings` — lexical binding types (P3: binding graph foundation).
//! - `dataflow` — dataflow types (P3: DataNode → DataNode, NOT SymbolId).
//!
//! ## Invariants
//! - IDs are deterministically derived (same inputs → same [u8; 32]).
//! - References are preserved after resolution (via `resolved: Option<...>`).
//! - All semantic edges carry `Confidence` and `Provenance`.
//! - DataNode → DataNode edges form the dataflow graph (NOT SymbolId).

pub mod ids;
pub mod enums;
pub mod structs;
pub mod bindings;
pub mod dataflow;

// --- IDs ---
pub use ids::{
    BindingId, BindingUseId, CallsiteId, DataFlowEdgeId, DataNodeId, EdgeId, FileId, ImportId,
    ReferenceId, ScopeId, SymbolId,
};

// --- Enums ---
pub use enums::{
    BindingKind, DataFlowKind, DataNodeKind, EdgeKind, ImportKind, Language, ParseStatus,
    Provenance, ReferenceKind, ResolutionStatus, ResolutionStrategy, ScopeKind, SymbolKind,
    Visibility,
};
pub use enums::Confidence;

// --- Core IR ---
pub use structs::{
    ArgumentFact, Callsite, DiagnosticLevel, ExtractionError, ExtractDiagnostic, FailureCategory,
    FileFacts, FileInfo, ImportDef, IndexReport, RawEdge, ReferenceUse, ResolvedTarget, ScopeDef,
    SymbolDef, TextRange,
};

// --- Bindings (P3) ---
pub use bindings::{BindingDef, BindingUse};

// --- Dataflow (P3) ---
pub use dataflow::{CallsiteArg, DataFlowEdge, DataNode};

// --- Utilities ---

/// Levenshtein edit distance between two strings (character-level).
/// Canonical implementation used by both search and resolution modules.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();

    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }

    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];

    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1)
                .min(curr[j - 1] + 1)
                .min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[m]
}
