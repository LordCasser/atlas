//! FocusGraphBuilder — constructs graph edges from focus closure resolutions.
//!
//! Unlike [`GraphBuilder::build_for_files`] which reads in-memory resolved pairs
//! from the full-index resolution pipeline, this builder reads from the
//! `reference_resolutions` DB table and applies [`EdgeConflictPolicy`] to
//! decide where each edge is written:
//!
//! | Confidence | Destination |
//! |-----------|-------------|
//! | Certain / High | `symbol_edges` (canonical) |
//! | Medium    | `symbol_edge_candidates` |
//! | Low       | Not persisted (gap) |
//!
//! Focus graph edges from closures use `ClosureComplete` or `Boundary` coverage
//! — never [`CoverageTier::RepoComplete`].

use std::sync::Arc;

use db::{CandidateEdge, ClosureResolution, Store};
use graph::GraphBuilderStats;
use types::enums::{EdgeKind, Provenance, ReferenceKind, ResolutionStrategy, SymbolKind};
use types::ids::EdgeId;
use types::structs::{CoverageTier, AnswerQuality, SemanticConfidence, SymbolTier};
use types::{Confidence, RawEdge, SymbolId};

use super::edge_policy::{EdgeConflictPolicy, EdgeResolution};

/// Builder that constructs graph edges from focus closure reference resolutions.
///
/// Operates independently from the core [`GraphBuilder`] — reads from the
/// `reference_resolutions` DB table instead of in-memory resolved pairs, and
/// applies [`EdgeConflictPolicy`] to route edges to the correct destination.
pub struct FocusGraphBuilder {
    store: Arc<Store>,
}

impl FocusGraphBuilder {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// Build graph edges from focus closure resolutions.
    ///
    /// # Flow
    /// 1. Loads all visible reference resolutions for the closure from
    ///    `reference_resolutions`.
    /// 2. For each resolution, loads the reference + target symbol,
    ///    derives the edge kind, and builds a [`AnswerQuality`].
    /// 3. Checks for existing canonical edges via conflict policy.
    /// 4. Routes each edge to `symbol_edges` (canonical) or
    ///    `symbol_edge_candidates` (candidate).
    ///
    /// # Returns
    /// [`GraphBuilderStats`] with counts of edges built and written to
    /// `symbol_edges`.  Candidate edges are tracked separately via the
    /// return value.
    pub fn build_for_closure(
        &self,
        closure_id: &str,
        generation: i64,
    ) -> anyhow::Result<FocusBuildResult> {
        // 1. Load all visible resolutions for this closure
        let resolutions = self.store.get_visible_resolutions_for_closure(closure_id)?;

        if resolutions.is_empty() {
            return Ok(FocusBuildResult {
                stats: GraphBuilderStats {
                    edges_built: 0,
                    edges_written: 0,
                    warnings: Vec::new(),
                },
                candidate_count: 0,
            });
        }

        // A reference has one canonical resolution within a committed focus
        // view. Remove focus-derived targets selected by older closures before
        // materializing this closure; repository-complete edges are preserved.
        self.store.delete_superseded_focus_edges(closure_id)?;

        let mut canonical_edges: Vec<RawEdge> = Vec::new();
        let mut candidate_rows: Vec<CandidateEdge> = Vec::new();
        let mut warnings: Vec<String> = Vec::new();
        let mut edges_built: usize = 0;

        for res in &resolutions {
            // 2a. Look up the reference
            let reference = match self.store.get_reference_by_id(&res.reference_id)? {
                Some(r) => r,
                None => {
                    warnings.push(format!(
                        "reference {:?} not found for closure {closure_id}",
                        res.reference_id
                    ));
                    continue;
                }
            };

            // 2b. Look up the target symbol
            let target_bytes: [u8; 32] = match res.target_symbol_id.as_slice().try_into() {
                Ok(b) => b,
                Err(_) => {
                    warnings.push("invalid target_symbol_id blob length".to_string());
                    continue;
                }
            };
            let target_sym_id = SymbolId::from_bytes(target_bytes);
            let target_sym = match self.store.find_symbol_by_id(&target_sym_id)? {
                Some(s) => s,
                None => {
                    warnings.push(format!(
                        "target symbol {:?} not found for closure {closure_id}",
                        res.target_symbol_id
                    ));
                    continue;
                }
            };

            // 2c. Derive edge kind from reference + target
            let edge_kind = match derive_edge_kind(&reference.kind, &target_sym.kind) {
                Some(k) => k,
                None => continue, // e.g. call reference to non-callable symbol
            };

            // Source symbol
            let source = match reference.source_symbol {
                Some(s) => s,
                None => {
                    warnings.push(format!(
                        "reference {:?} has no source_symbol; skipping",
                        reference.id
                    ));
                    continue;
                }
            };

            // 2d. Build AnswerQuality for the incoming edge
            let incoming_precision =
                build_incoming_precision(closure_id, &res.coverage_tier, &res.semantic_confidence);

            // 2e. Check existing canonical edge
            let existing =
                self.store
                    .find_edge_by_source_target_kind(&source, &target_sym_id, &edge_kind)?;

            let existing_precision: Option<AnswerQuality> = existing
                .as_ref()
                .map(|e| edge_to_precision(&e.provenance, e.confidence));

            // 2f. Apply conflict policy
            let resolution =
                EdgeConflictPolicy::resolve(existing_precision.as_ref(), &incoming_precision, None);

            // 2g. Route based on resolution
            match resolution {
                EdgeResolution::Keep => {
                    // Existing edge is better or equal — skip
                    continue;
                }
                EdgeResolution::Replace => {
                    // Write as canonical edge
                    let confidence = semantic_confidence_to_f32(&incoming_precision.confidence);
                    let provenance = build_focus_provenance(closure_id, generation, res);

                    let mut edge = RawEdge::new(
                        EdgeId::generate(
                            &source,
                            &target_sym_id,
                            edge_kind.as_str(),
                            Some(&reference.id),
                            provenance.as_str(),
                        ),
                        source,
                        target_sym_id,
                        edge_kind,
                        Confidence::new(confidence),
                        provenance,
                    );
                    edge.ref_id = Some(reference.id);
                    edge.location = Some(reference.range);
                    edge.resolved_by = Some(
                        ResolutionStrategy::from_str(&res.resolution_strategy)
                            .unwrap_or(ResolutionStrategy::ExactMatch),
                    );
                    canonical_edges.push(edge);
                    edges_built += 1;
                }
                EdgeResolution::KeepAsCandidates => {
                    // Write as candidate edge
                    candidate_rows.push(CandidateEdge {
                        source: source.as_bytes().to_vec(),
                        target: Some(target_sym_id.as_bytes().to_vec()),
                        kind: edge_kind.as_str().to_string(),
                        coverage_tier: res.coverage_tier.clone(),
                        semantic_confidence: res.semantic_confidence.clone(),
                        candidate_count: None,
                        closure_id: closure_id.to_string(),
                        generation,
                    });
                    edges_built += 1;
                }
            }
        }

        // 3. Write canonical edges
        let edges_written = if !canonical_edges.is_empty() {
            match self.store.insert_edges(&canonical_edges) {
                Ok(()) => canonical_edges.len(),
                Err(e) => {
                    warnings.push(format!("canonical edge batch insert failed: {e}"));
                    0
                }
            }
        } else {
            0
        };

        // 4. Write candidate edges
        let candidate_count = if !candidate_rows.is_empty() {
            match self.store.batch_insert_candidate_edges(&candidate_rows) {
                Ok(n) => n,
                Err(e) => {
                    warnings.push(format!("candidate edge batch insert failed: {e}"));
                    0
                }
            }
        } else {
            0
        };

        Ok(FocusBuildResult {
            stats: GraphBuilderStats {
                edges_built,
                edges_written,
                warnings,
            },
            candidate_count,
        })
    }
}

/// Result of a focus closure graph build.
#[derive(Debug, Clone)]
pub struct FocusBuildResult {
    /// Statistics for canonical (`symbol_edges`) edges.
    pub stats: GraphBuilderStats,
    /// Number of candidate edges written to `symbol_edge_candidates`.
    pub candidate_count: usize,
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Derive the graph edge kind from a reference kind and target symbol kind.
///
/// Mirrors the logic in [`GraphBuilder::create_edges_for_reference`] but
/// without the dataflow/intra-procedural resolution branches.
fn derive_edge_kind(ref_kind: &ReferenceKind, target_kind: &SymbolKind) -> Option<EdgeKind> {
    if *ref_kind == ReferenceKind::Call {
        match target_kind {
            SymbolKind::Class | SymbolKind::Struct => Some(EdgeKind::Instantiates),
            SymbolKind::Interface | SymbolKind::Trait => Some(EdgeKind::Implements),
            SymbolKind::Function | SymbolKind::Method | SymbolKind::Constructor => {
                Some(EdgeKind::Calls)
            }
            _ => None, // Non-callable target — skip
        }
    } else if *ref_kind == ReferenceKind::Inheritance {
        Some(EdgeKind::Extends)
    } else if *ref_kind == ReferenceKind::Implementation {
        Some(EdgeKind::Implements)
    } else {
        Some(EdgeKind::References)
    }
}

/// Convert the string columns from `reference_resolutions` into a [`AnswerQuality`].
///
/// Focus graph edges always use `ClosureComplete` or `Boundary` coverage
/// — never `RepoComplete`.
fn build_incoming_precision(
    closure_id: &str,
    coverage_tier: &str,
    semantic_confidence: &str,
) -> AnswerQuality {
    let coverage = match coverage_tier {
        "closure_complete" => CoverageTier::ClosureComplete {
            closure_id: closure_id.to_string(),
        },
        "boundary" => CoverageTier::Boundary {
            target_tier: SymbolTier::Partial,
        },
        "partial" => CoverageTier::Partial { gaps: vec![] },
        "manifest" => CoverageTier::Manifest,
        _ => {
            // Unknown tier — treat as Boundary with Partial symbol tier
            CoverageTier::Boundary {
                target_tier: SymbolTier::Partial,
            }
        }
    };

    let confidence = match semantic_confidence {
        "certain" => SemanticConfidence::Certain,
        "high" => SemanticConfidence::High,
        "medium" => SemanticConfidence::Medium,
        _ => SemanticConfidence::Low,
    };

    AnswerQuality {
        coverage,
        confidence,
    }
}

/// Derive a [`AnswerQuality`] from an existing edge's provenance and f32 confidence.
///
/// - `tree_sitter` → `RepoComplete` (from full index)
/// - Everything else → `Boundary` (conservative — we cannot distinguish
///   focus closures from other heuristics in the stored provenance enum)
fn edge_to_precision(provenance: &Provenance, confidence: Confidence) -> AnswerQuality {
    let coverage = if provenance.as_str() == "tree_sitter" {
        CoverageTier::RepoComplete
    } else {
        CoverageTier::Boundary {
            target_tier: SymbolTier::Partial,
        }
    };

    let semantic = if confidence.as_f32() >= 0.9 {
        SemanticConfidence::Certain
    } else if confidence.as_f32() >= 0.7 {
        SemanticConfidence::High
    } else if confidence.as_f32() >= 0.5 {
        SemanticConfidence::Medium
    } else {
        SemanticConfidence::Low
    };

    AnswerQuality {
        coverage,
        confidence: semantic,
    }
}

/// Map [`SemanticConfidence`] to an f32 for the [`RawEdge::confidence`] field.
fn semantic_confidence_to_f32(sc: &SemanticConfidence) -> f32 {
    match sc {
        SemanticConfidence::Certain => 0.9,
        SemanticConfidence::High => 0.7,
        SemanticConfidence::Medium => 0.5,
        SemanticConfidence::Low => 0.1,
    }
}

/// Build a provenance value for a focus closure edge.
///
/// Constructs a [`Provenance::FocusClosure`] carrying the closure identity
/// so that downstream consumers can distinguish focus-built edges from
/// full-index (RepoComplete) edges.
fn build_focus_provenance(
    closure_id: &str,
    _generation: i64,
    _res: &ClosureResolution,
) -> Provenance {
    Provenance::FocusClosure(closure_id.to_string())
}
