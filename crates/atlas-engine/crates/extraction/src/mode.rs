//! Extraction mode: controls which analysis phases `extract_file_with_mode` executes.
//!
//! This is an internal enum — it is NOT exposed outside the `extraction` crate.
//! External callers (CLI, lazy crate) pass an `ExtractionMode` value through
//! the `extract_file_with_mode()` function signature.

use types::lazy::LazyWindow;

/// Controls the set of extraction phases executed by `extract_file_with_mode`.
///
/// # Phase matrix
///
/// | Phase                | Manifest | ResSym | Structural | LazyDataflow | Full |
/// |----------------------|:--------:|:------:|:----------:|:------------:|:----:|
/// | 1. parse            | ✓        | ✓      | ✓          | ✓            | ✓    |
/// | 2. symbols          | top-level| ✓      | ✓          | ✗ (reuse)    | ✓    |
/// | 3. references       | ✗        | ✗      | ✓          | ✗ (reuse)    | ✓    |
/// | 4. imports          | ✗        | ✓      | ✓          | ✗ (reuse)    | ✓    |
/// | 5. scopes           | ✗        | ✓      | ✓          | ✗ (reuse)    | ✓    |
/// | 7. scope_tree       | ✗        | ✓      | ✓          | ✗ (reuse)    | ✓    |
/// | 7a. lexical_bindings| ✗        | ✗      | ✓          | window only   | ✓    |
/// | 7b. dataflow        | ✗        | ✗      | ✗          | window only   | ✓    |
/// | 7c. use-def         | ✗        | ✗      | ✗          | window only   | ✓    |
/// | 7e. cfg             | ✗        | ✗      | ✗          | window only   | ✓    |
/// | 8. semantic_bind    | ✗        | ✗      | ✓          | ✗ (reuse)    | ✓    |
/// | 8a. ref binding uses| ✗        | ✗      | ✗ (skip)   | window only   | ✓    |
/// | 9. callsites        | ✗        | ✗      | ✓          | ✗ (reuse)    | ✓    |
/// | 9a. backfill        | ✗        | ✗      | partial*   | data_node_id  | ✓    |
/// | 10. exports         | ✗        | ✗      | ✓          | ✗ (reuse)    | ✓    |
///
/// *Manifest: only top-level symbols (file-scope declarations), no references/scopes/etc.
/// *ResSym: all symbols + imports + scopes + scope_tree; no references/dataflow/callsites.
/// *Structural: backfills callsite range/callee only (no data_node_id).
#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum ExtractionMode {
    /// Lightweight manifest mode (`--analysis manifest`).
    ///
    /// Produces only top-level symbols (file-scope function/struct/enum/typedef
    /// declarations).  Does NOT produce references, scopes, imports, dataflow,
    /// cfg, callsites, or exports.  Intended for fast global candidate indexing
    /// that feeds the query-driven LazyStructuralService.
    Manifest,

    /// Resolution-symbols mode for dependency resolution.
    ///
    /// Produces ALL symbols (not just top-level), scopes, scope tree,
    /// and imports. Does NOT produce references, dataflow, CFG, callsites,
    /// or exports. Intended as a lightweight alternative to full structural
    /// extraction for import dependency files that only need to be present
    /// as resolution targets.
    ResolutionSymbols,

    /// Default indexing mode.
    ///
    /// Produces: symbols, references, imports, scopes, scope tree,
    /// lexical bindings (7a only, no 8a identifier scan), callsites,
    /// and exports.  Does NOT produce: data nodes, dataflow edges,
    /// use-def edges, CFG nodes/edges, or identifier-use binding uses (8a).
    Structural,

    /// Internal lazy-load mode.
    ///
    /// Only builds dataflow for functions listed in `window` that belong to
    /// the current file.  Structural fields (symbols, references, scopes,
    /// callsites) are left empty — the caller is responsible for writing only
    /// the dataflow-related fields to the database.
    LazyDataflow {
        /// The window of AnalysisUnits to build dataflow for.
        window: LazyWindow,
    },

    /// Full analysis mode (`--analysis full`).
    ///
    /// Executes every extraction phase.  Produces complete FileFacts
    /// including all dataflow and CFG data.  This is the pre-lazy behavior.
    Full,
}

impl ExtractionMode {
    /// Whether manifest-level facts (top-level symbols only) should be produced.
    pub fn produces_manifest(&self) -> bool {
        matches!(self, Self::Manifest)
    }

    /// Whether references should be extracted.
    pub fn produces_references(&self) -> bool {
        matches!(self, Self::Structural | Self::Full)
    }

    /// Whether structural facts (symbols, references, scopes, imports,
    /// callsites, exports) should be produced.
    pub fn produces_structural(&self) -> bool {
        matches!(self, Self::Structural | Self::Full)
    }

    /// Whether dataflow facts (data nodes, dataflow edges, use-def edges)
    /// should be produced.
    pub fn produces_dataflow(&self) -> bool {
        matches!(self, Self::LazyDataflow { .. } | Self::Full)
    }

    /// Whether CFG nodes/edges should be produced.
    pub fn produces_cfg(&self) -> bool {
        matches!(self, Self::LazyDataflow { .. } | Self::Full)
    }

    /// Whether full identifier-use binding scan (step 8a) should run.
    pub fn produces_reference_binding_uses(&self) -> bool {
        matches!(self, Self::LazyDataflow { .. } | Self::Full)
    }
}

/// Parse an analysis mode string from CLI/MCP arguments.
///
/// Accepts `"manifest"`, `"structural"`, or `"full"` and returns an error
/// with a descriptive message for any other value.
pub fn parse_analysis_mode(s: &str) -> anyhow::Result<ExtractionMode> {
    match s {
        "manifest" => Ok(ExtractionMode::Manifest),
        "structural" => Ok(ExtractionMode::Structural),
        "full" => Ok(ExtractionMode::Full),
        other => anyhow::bail!(
            "Unknown analysis mode: '{other}'. Must be 'manifest', 'structural', or 'full'."
        ),
    }
}

// ── Lazy dataflow budget constants (internal, per-unit caps) ──────────────
//
// These mirror the constants in the `lazy` crate.  They are duplicated here
// because `extraction` cannot depend on `lazy`.  Values must be kept in sync.
//
// Not exposed to MCP tools, CLI parameters, or external configuration.

/// Maximum DataNode count for a single AnalysisUnit.
pub(crate) const LAZY_MAX_NODES_PER_UNIT: usize = 2_000;

/// Maximum DataFlowEdge count for a single AnalysisUnit.
pub(crate) const LAZY_MAX_EDGES_PER_UNIT: usize = 20_000;
