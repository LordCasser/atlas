//! Lazy dataflow types: AnalysisUnit, LazyWindow, VariableFocus.
//!
//! These types define the granularity and scope of on-demand dataflow loading.
//! They are consumed by the `lazy` crate (Planner/Loader) and `extraction`
//! crate (ExtractionMode::LazyDataflow), but NOT by `analysis` or `db`.

use crate::ids::{FileId, ReferenceId, SymbolId};
use crate::structs::CapabilityMask;
use crate::structs::TextRange;
use crate::structs::precision::PrecisionTier;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// AnalysisUnit
// ---------------------------------------------------------------------------

/// The minimum granularity for lazy dataflow construction.
///
/// Each unit corresponds to either a named function or the top-level scope
/// of a file.  The `unit_id` is a deterministic 16-byte identifier suitable
/// for use as a non-nullable primary-key component in SQLite.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnalysisUnit {
    /// The file containing this unit.
    pub file_id: FileId,
    /// Stable 16-byte identifier (NEVER null — suitable for composite PK).
    /// For functions: derived from `symbol_id` bytes.
    /// For top-level: derived from `file_id` bytes with 0xFF marker.
    pub unit_id: [u8; 16],
    /// The function symbol, or `None` for top-level scope.
    pub symbol_id: Option<SymbolId>,
    /// Byte range of this unit in its source file.
    pub range: TextRange,
}

impl AnalysisUnit {
    /// Create an AnalysisUnit for a named function.
    pub fn from_function(file_id: FileId, symbol_id: SymbolId, range: TextRange) -> Self {
        let mut unit_id = [0u8; 16];
        let bytes = symbol_id.as_bytes();
        unit_id.copy_from_slice(&bytes[..16]);
        Self {
            file_id,
            unit_id,
            symbol_id: Some(symbol_id),
            range,
        }
    }

    /// Create an AnalysisUnit for top-level (file-scoped) code.
    pub fn from_top_level(file_id: FileId, range: TextRange) -> Self {
        let mut unit_id = [0u8; 16];
        let bytes = file_id.as_bytes();
        unit_id.copy_from_slice(&bytes[..16]);
        // XOR first byte to distinguish from function-derived IDs
        unit_id[0] ^= 0xFF;
        Self {
            file_id,
            unit_id,
            symbol_id: None,
            range,
        }
    }

    /// Whether this unit represents a top-level scope (no enclosing function).
    pub fn is_top_level(&self) -> bool {
        self.symbol_id.is_none()
    }
}

// ---------------------------------------------------------------------------
// LazyWindow
// ---------------------------------------------------------------------------

/// Planner output: the set of AnalysisUnits that should have dataflow built
/// for a given query, ordered by proximity (depth 0 first).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LazyWindow {
    /// The unit closest to the query origin.
    pub seed_unit: AnalysisUnit,
    /// All units to build, ordered depth 0 → 1 → 2.
    pub units: Vec<AnalysisUnit>,
    /// Variable-level focus for more precise range filtering (trace_variable only).
    pub variable_focus: Option<VariableFocus>,
    /// Whether this window was truncated by a hard budget cap.
    pub truncated: bool,
    /// Units whose dataflow was built from scratch (populated at runtime).
    #[serde(default)]
    pub units_built: usize,
    /// Units whose dataflow was already cached (populated at runtime).
    #[serde(default)]
    pub units_cached: usize,
    /// Units skipped because another request is currently building them.
    #[serde(default)]
    pub units_pending: usize,
    /// Active job ids for pending units, useful for MCP retry/status hints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_job_ids: Vec<String>,
    /// Dataflow precision tier (set by LazyDataflowService after loading).
    /// None if dataflow was not loaded via lazy path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision_tier: Option<PrecisionTier>,
    /// Capability mask for this window's units (summarized from extraction_state layers).
    #[serde(default, skip_serializing_if = "CapabilityMask::is_zero")]
    pub capability_mask: CapabilityMask,
}

// ---------------------------------------------------------------------------
// VariableFocus
// ---------------------------------------------------------------------------

/// Extra metadata when a trace query targets a specific variable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableFocus {
    /// Simple name of the variable (e.g. "count").
    pub name: String,
    /// The byte range of the reference the user clicked on.
    pub reference_range: TextRange,
    /// The reference ID, if the position matched a known reference.
    pub reference_id: Option<ReferenceId>,
    /// The resolved target symbol, if the reference was resolved.
    pub resolved_symbol_id: Option<SymbolId>,
}
