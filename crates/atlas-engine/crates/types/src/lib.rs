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
pub mod effects;
pub mod enums;
pub mod ids;
pub mod lazy;
pub mod progress;
pub mod structs;
pub mod summary;
pub mod timing;
pub mod trace;

// --- Effects ---
pub use effects::{
    ConsumptionContract, ConsumptionStyle, EscapeTarget, OwnershipContract, PlaceRef,
    ResourceLocator, ReturnContract, SemanticEffect, SemanticEffectKind, ValueSource,
};

// --- IDs ---
pub use ids::{
    BindingId, BindingUseId, CallsiteId, CfgEdgeId, CfgNodeId, DataFlowEdgeId, DataNodeId, EdgeId,
    EffectId, FileId, ImportId, ReferenceId, ScopeId, SymbolId,
};

// --- Enums ---
pub use enums::Confidence;
pub use enums::{
    BindingKind, CallContext, CfgEdgeKind, CfgNodeKind, DataFlowKind, DataNodeKind, EdgeKind,
    EffectKind, ImportKind, Language, ParseStatus, Provenance, ReferenceKind, ResolutionStatus,
    ResolutionStrategy, ScopeKind, SymbolKind, Visibility,
};

// --- Core IR ---
pub use structs::{
    ArgumentFact, Callsite, CapabilityMask, DiagnosticLevel, ExtractDiagnostic, ExtractionError,
    FailureCategory, FileFacts, FileInfo, FpAnnotation, ImportDef, IndexReport, RawEdge,
    ReferenceUse, ResolvedCallsite, ResolvedTarget, ScopeDef, SymbolDef, TextRange,
    canonicalize_field_path, layer, status,
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
    BoundaryKind, BoundaryMarker, Evidence, LazySummary, TraceDataNodeRef, TraceDiagnostic,
    TracePath, TracePathStep, TracePoint, VariableTracePath,
};

// --- Caller path types ---
pub use caller_path::{CallerChain, CallerChainStep, CallerPath, ForwardChain, ForwardChainStep};

// --- Summary types ---
pub use summary::{CallArgFlow, FunctionSummary, ParameterFlow, ReturnFlow};

// --- Timing types ---
pub use timing::{LanguageEntry, PerLanguageStats, PhaseTimer, PhaseTiming, PhaseTimings};

// --- Lazy types ---
pub use lazy::{AnalysisUnit, LazyWindow, StaleStructuralIndexError, VariableFocus};

// --- Progress types ---
pub use progress::{
    CompletedPhase, PhaseEntry, PhaseState, ProgressPhase, ProgressSnapshot, ProgressState,
};

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

/// Levenshtein distance with early termination when distance exceeds `max_dist`.
/// Returns `None` if distance > max_dist, `Some(dist)` otherwise.
///
/// Uses:
/// - Length pruning: if |len(a) - len(b)| > max_dist, return `None` immediately.
/// - Row-min early termination: once every cell in a row exceeds max_dist, stop.
/// - ASCII fast path: 95%+ of symbol names are ASCII, uses byte-level comparison
///   which is measurably faster than char-level iteration.
pub fn levenshtein_bounded(a: &str, b: &str, max_dist: usize) -> Option<usize> {
    let a_len = a.chars().count();
    let b_len = b.chars().count();

    // Length prune: distance >= |len(a) - len(b)|
    if a_len.abs_diff(b_len) > max_dist {
        return None;
    }

    // Fast path: ASCII-only (95%+ of symbol names)
    if a.is_ascii() && b.is_ascii() {
        return levenshtein_bounded_bytes(a.as_bytes(), b.as_bytes(), max_dist);
    }

    // Unicode fallback
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    levenshtein_bounded_chars(&a_chars, &b_chars, max_dist)
}

fn levenshtein_bounded_bytes(a: &[u8], b: &[u8], max_dist: usize) -> Option<usize> {
    let n = a.len();
    let m = b.len();
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];

    for i in 1..=n {
        curr[0] = i;
        let mut row_min = curr[0];
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(curr[j]);
        }
        // EARLY TERMINATION: entire row min > max_dist
        if row_min > max_dist {
            return None;
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    let dist = prev[m];
    if dist <= max_dist { Some(dist) } else { None }
}

fn levenshtein_bounded_chars(a: &[char], b: &[char], max_dist: usize) -> Option<usize> {
    let n = a.len();
    let m = b.len();
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr = vec![0usize; m + 1];

    for i in 1..=n {
        curr[0] = i;
        let mut row_min = curr[0];
        for j in 1..=m {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
            row_min = row_min.min(curr[j]);
        }
        if row_min > max_dist {
            return None;
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    let dist = prev[m];
    if dist <= max_dist { Some(dist) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levenshtein_basic() {
        // Existing function must still work correctly.
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("foo", "foo"), 0);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", ""), 3);
    }

    #[test]
    fn levenshtein_bounded_within_limit() {
        // Distance 1, max_dist=2 → Some(1)
        assert_eq!(levenshtein_bounded("foo", "foe", 2), Some(1));
    }

    #[test]
    fn levenshtein_bounded_exceeds_limit() {
        // Distance 3, max_dist=2 → None (row-min early termination)
        assert_eq!(levenshtein_bounded("foo", "abcdef", 2), None);
    }

    #[test]
    fn levenshtein_bounded_length_prune() {
        // Length diff 5 > max_dist 2 → None (early length prune)
        assert_eq!(levenshtein_bounded("a", "abcdef", 2), None);
    }

    #[test]
    fn levenshtein_bounded_zero_distance() {
        assert_eq!(levenshtein_bounded("hello", "hello", 2), Some(0));
    }

    #[test]
    fn levenshtein_bounded_unicode() {
        // Non-ASCII path (Unicode char-level fallback)
        assert_eq!(levenshtein_bounded("héllo", "héllö", 2), Some(1));
    }

    #[test]
    fn levenshtein_bounded_unicode_exceeds() {
        // Unicode with distance exceeding threshold
        assert_eq!(levenshtein_bounded("héllo", "wörld", 1), None);
    }
}
