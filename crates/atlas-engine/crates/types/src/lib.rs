//! Atlas core type system: IDs, enums, and the Intermediate Representation (IR).
//!
//! ## Layering
//! - `ids`  — 14 typed blake3 newtypes stored as BLOB in SQLite.
//! - `enums` — 17 enum families describing language, kind, visibility, etc.
//! - `structs` — the core IR: SymbolDef, ReferenceUse, FileFacts, etc.
//! - `bindings` — lexical binding graph types.
//! - `dataflow` — per-function dataflow types (DataNode → DataNode, NOT SymbolId).
//! - `cfg` — per-function control-flow graph types.
//! - `capability` — per-language analysis capability profiles.
//! - `trace` — location-driven trace types (TracePoint, TracePath).
//! - `caller_path` — reverse call-graph traversal types (CallerChain/CallerPath).
//! - `summary` — lightweight intraprocedural function summaries.
//! - `timing` — phase timing and per-language statistics for index pipeline.
//!
//! ## Invariants
//! - IDs are deterministically derived (same inputs → same [u8; 32]).
//! - References are preserved after resolution (via `resolved: Option<...>`).
//! - All semantic edges carry `Confidence` and `Provenance`.
//! - DataNode → DataNode edges form the dataflow graph (NOT SymbolId).
//! - CfgNode → CfgNode edges form the control-flow graph (per-function).

pub mod bindings;
pub mod caller_path;
pub mod capability;
pub mod cfg;
pub mod dataflow;
pub mod enums;
pub mod ids;
pub mod lazy;
pub mod structs;
pub mod summary;
pub mod timing;
pub mod trace;

// --- IDs ---
pub use ids::{
    BindingId, BindingUseId, CallsiteId, CfgEdgeId, CfgNodeId, DataFlowEdgeId, DataNodeId, EdgeId,
    FileId, ImportId, ReferenceId, ScopeId, SymbolId,
};

// --- Enums ---
pub use enums::Confidence;
pub use enums::{
    BindingKind, CfgEdgeKind, CfgNodeKind, DataFlowKind, DataNodeKind, EdgeKind, ImportKind,
    Language, ParseStatus, Provenance, ReferenceKind, ResolutionStatus, ResolutionStrategy,
    ScopeKind, SymbolKind, Visibility,
};

// --- Core IR ---
pub use structs::{
    ArgumentFact, Callsite, DiagnosticLevel, ExtractDiagnostic, ExtractionError, FailureCategory,
    FileFacts, FileInfo, ImportDef, IndexReport, RawEdge, ReferenceUse, ResolvedTarget, ScopeDef,
    SymbolDef, TextRange, layer, status,
};

// --- Binding types ---
pub use bindings::{BindingDef, BindingUse};

// --- Dataflow types ---
pub use dataflow::{DataFlowEdge, DataNode};

// --- CFG types ---
pub use cfg::{CfgEdge, CfgNode};

// --- Capability types ---
pub use capability::{CapabilityLevel, FeatureMatrix, FeatureSupport, LanguageCapabilityProfile};

// --- Trace types ---
pub use trace::{
    Evidence, LazySummary, TraceDataNodeRef, TraceDiagnostic, TracePath, TracePathStep, TracePoint,
    VariableTracePath,
};

// --- Caller path types ---
pub use caller_path::{CallerChain, CallerChainStep, CallerPath};

// --- Summary types ---
pub use summary::{CallArgFlow, FunctionSummary, ParameterFlow, ReturnFlow};

// --- Timing types ---
pub use timing::{LanguageEntry, PerLanguageStats, PhaseTimer, PhaseTiming, PhaseTimings};

// --- Lazy types ---
pub use lazy::{AnalysisUnit, LazyWindow, VariableFocus};

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
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[m]
}
