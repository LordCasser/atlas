//! Core focus-driven analysis types.
//!
//! These types form the foundation of the Atlas focus system. A user
//! provides a [`FocusSeed`] (what they're looking at), wrapped in a
//! [`FocusWindow`] with expansion strategies and a [`WindowBudget`].
//! The system builds a [`FocusClosure`] — the set of files and symbols
//! needed to answer queries about the seed.
//!
//! ## Relationship to existing types
//!
//! - [`FocusSeed`] upgrades [`InvestigationFocus`] with `File` variant + `language`.
//! - [`WindowBudget`] extends the existing lazy budget concepts.
//! - [`KnownGap`] is shared with the [`Precision`] model in `types::structs`.

use std::collections::HashSet;

use crate::closure_planner::IncludeRoot;

use types::enums::{Language, SymbolKind};
use types::ids::{FileId, SymbolId};
use types::structs::KnownGap;
use types::structs::Precision;

// ---------------------------------------------------------------------------
// Direction — expansion direction for graph-based strategies
// ---------------------------------------------------------------------------

/// Direction of traversal for call-graph and type-graph expansion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Follow outgoing edges (callees, used types).
    Outgoing,
    /// Follow incoming edges (callers, users of a type).
    Incoming,
    /// Follow both directions.
    Both,
}

// ---------------------------------------------------------------------------
// FocusSeed — what the user is looking at
// ---------------------------------------------------------------------------

/// What the user is looking at — the seed for focus analysis.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FocusSeed {
    /// A named symbol (function, struct, class, etc.).
    Symbol {
        name: String,
        kind: Option<SymbolKind>,
        language: Language,
        file_id: Option<FileId>,
    },
    /// A specific source position.
    Position {
        file_id: FileId,
        line: u32,
        column: u32,
    },
    /// A struct/class field.
    Field {
        struct_sym: SymbolId,
        field_path: String,
    },
    /// An entire file.
    File { file_id: FileId, language: Language },
}

// ---------------------------------------------------------------------------
// ClosureStrategy — how to expand focus
// ---------------------------------------------------------------------------

/// Strategy for expanding the focus closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClosureStrategy {
    /// Expand through imports/includes to the specified depth.
    ImportNeighborhood { depth: u32 },
    /// Include sibling files in the same directory.
    SameDirectory,
    /// Expand through call graph (callers or callees).
    CallGraph { direction: Direction, depth: u32 },
    /// Expand through type definitions.
    TypeGraph { max_depth: u32 },
}

// ---------------------------------------------------------------------------
// WindowBudget — limits for expansion
// ---------------------------------------------------------------------------

/// Budget for a focus window — limits expansion.
#[derive(Debug, Clone)]
pub struct WindowBudget {
    pub max_files: usize,
    pub max_time_ms: u64,
    pub max_fanout_per_name: usize,
    pub max_iterations: u32,
}

impl Default for WindowBudget {
    fn default() -> Self {
        WindowBudget {
            max_files: 30,
            max_time_ms: 18_000,
            max_fanout_per_name: 20,
            max_iterations: 3,
        }
    }
}

impl WindowBudget {
    /// Budget suitable for background speculative work.
    pub fn background() -> Self {
        WindowBudget {
            max_files: 100,
            max_time_ms: 60_000,
            max_fanout_per_name: 20,
            max_iterations: 1,
        }
    }
}

// ---------------------------------------------------------------------------
// FocusWindow — seed + strategies + budget
// ---------------------------------------------------------------------------

/// A focus window — seed + strategies + budget.
#[derive(Debug, Clone)]
pub struct FocusWindow {
    pub seed: FocusSeed,
    pub strategies: Vec<ClosureStrategy>,
    pub include_roots: Vec<IncludeRoot>,
    pub budget: WindowBudget,
    pub language: Language,
    pub max_iterations: u32,
}

impl FocusWindow {
    pub fn new(seed: FocusSeed, language: Language) -> Self {
        FocusWindow {
            seed,
            strategies: vec![ClosureStrategy::ImportNeighborhood { depth: 2 }],
            include_roots: Vec::new(),
            budget: WindowBudget::default(),
            language,
            max_iterations: 3,
        }
    }
}

// ---------------------------------------------------------------------------
// FocusClosure — the built closure
// ---------------------------------------------------------------------------

/// The result of building a focus closure.
#[derive(Debug, Clone)]
pub struct FocusClosure {
    pub seed: FocusSeed,
    pub files: HashSet<FileId>,
    pub symbols: HashSet<SymbolId>,
    pub visited: HashSet<FileId>,
    pub gaps: Vec<KnownGap>,
    /// Raw extraction jobs encountered through LazyStructuralService
    /// in-flight de-duplication. These are retryable work, not terminal gaps.
    pub pending_extraction_job_ids: Vec<String>,
}

impl FocusClosure {
    pub fn new(seed: &FocusSeed) -> Self {
        FocusClosure {
            seed: seed.clone(),
            files: HashSet::new(),
            symbols: HashSet::new(),
            visited: HashSet::new(),
            gaps: Vec::new(),
            pending_extraction_job_ids: Vec::new(),
        }
    }

    pub fn mark_extracted(&mut self, file_id: FileId, _precision: &Precision) {
        self.files.insert(file_id);
        self.visited.insert(file_id);
    }

    pub fn record_gap(&mut self, gap: KnownGap) {
        self.gaps.push(gap);
    }

    pub fn record_pending_extraction_jobs(&mut self, job_ids: impl IntoIterator<Item = String>) {
        for job_id in job_ids {
            if !self.pending_extraction_job_ids.contains(&job_id) {
                self.pending_extraction_job_ids.push(job_id);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// FocusJobState — lifecycle tracking
// ---------------------------------------------------------------------------

/// State of a focus job (currently always Planned — lifecycle tracking TBD).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusJobState {
    Planned,
}
