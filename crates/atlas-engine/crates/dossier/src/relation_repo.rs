//! RelationRepository implementation backed by Store (RawEdge) + GraphSnapshot.
//!
//! This implements Option A (join-at-query-time): the graph snapshot provides
//! fast adjacency traversal and counts, while the Store provides full RawEdge
//! records with provenance, confidence, and source location.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use types::{EdgeKind, RawEdge, SymbolId, TextRange};

use super::traits::{RelationEvidence, RelationRepository};
use super::types::InternalRelationKind;

// ── RelationRepo ────────────────────────────────────────────────────────────

/// Relation evidence provider backed by Store + GraphSnapshot.
///
/// `store` provides full `RawEdge` records (with location, confidence,
/// provenance).  `graph` provides O(1) adjacency lookups for counts and
/// neighbor iteration.
pub struct RelationRepo {
    store: Arc<db::Store>,
    graph: Arc<graph::GraphEngine>,
}

impl RelationRepo {
    /// Create a new RelationRepo.
    pub fn new(store: Arc<db::Store>, graph: Arc<graph::GraphEngine>) -> Self {
        Self { store, graph }
    }
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// Build a minimal TextRange anchored at the given line (0-based).
fn fallback_range(line: u32) -> TextRange {
    TextRange {
        start_byte: 0,
        end_byte: 0,
        start_line: line,
        start_column: 0,
        end_line: line,
        end_column: 0,
    }
}

/// Filter graph neighbour candidates to those that map to a dossier-relevant
/// [`InternalRelationKind`], optionally further restricted by `kinds`.
fn filter_candidates(
    raw: Vec<(graph::NodeIx, EdgeKind)>,
    kinds: Option<&[InternalRelationKind]>,
    limit: usize,
) -> Vec<(graph::NodeIx, EdgeKind, InternalRelationKind)> {
    raw.into_iter()
        .filter_map(|(nix, ek)| {
            let irk = InternalRelationKind::from_edge_kind(ek)?;
            Some((nix, ek, irk))
        })
        .filter(|(_, _, irk)| kinds.is_none_or(|allowed| allowed.contains(irk)))
        .take(limit)
        .collect()
}

// ── RelationRepository impl ─────────────────────────────────────────────────

impl RelationRepository for RelationRepo {
    fn incoming_evidence(
        &self,
        symbol_id: &SymbolId,
        kinds: Option<&[InternalRelationKind]>,
        limit: usize,
    ) -> Result<Vec<RelationEvidence>> {
        let snapshot = self.graph.snapshot();

        let raw_pairs = snapshot.incoming_neighbors_with_kinds(symbol_id);
        let candidates = filter_candidates(raw_pairs, kinds, limit);

        if candidates.is_empty() {
            return Ok(vec![]);
        }

        // Fetch all incoming edges from the store once, then match in memory.
        let raw_edges: Vec<RawEdge> = self.store.find_edges_by_target(symbol_id)?;
        let edge_index: HashMap<(SymbolId, EdgeKind), &RawEdge> =
            raw_edges.iter().map(|e| ((e.source, e.kind), e)).collect();

        let mut results = Vec::with_capacity(candidates.len());
        for (source_ix, ek, irk) in candidates {
            let source_node = snapshot.node(source_ix);
            let source_id = source_node.symbol_id;

            let edge = match edge_index.get(&(source_id, ek)) {
                Some(e) => e,
                None => continue, // edge not in store (should not happen, but be defensive)
            };

            let range = edge
                .location
                .unwrap_or_else(|| fallback_range(source_node.start_line));

            results.push(RelationEvidence {
                source_id: edge.source,
                target_id: edge.target,
                relation_kind: irk,
                file_id: source_node.file_id,
                range,
                confidence: edge.confidence,
            });
        }

        Ok(results)
    }

    fn outgoing_evidence(
        &self,
        symbol_id: &SymbolId,
        kinds: Option<&[InternalRelationKind]>,
        limit: usize,
    ) -> Result<Vec<RelationEvidence>> {
        let snapshot = self.graph.snapshot();

        let raw_pairs = snapshot.outgoing_neighbors_with_kinds(symbol_id);
        let candidates = filter_candidates(raw_pairs, kinds, limit);

        if candidates.is_empty() {
            return Ok(vec![]);
        }

        // Fetch all outgoing edges from the store once, then match in memory.
        let raw_edges: Vec<RawEdge> = self.store.find_edges_by_source(symbol_id)?;
        let edge_index: HashMap<(SymbolId, EdgeKind), &RawEdge> =
            raw_edges.iter().map(|e| ((e.target, e.kind), e)).collect();

        // Subject node (for file_id fallback — the call site is in the subject's file).
        let subject_node = snapshot.node_by_id(symbol_id);
        let subject_file_id = subject_node.map(|n| n.file_id);
        let subject_start_line = subject_node.map(|n| n.start_line).unwrap_or(0);

        let mut results = Vec::with_capacity(candidates.len());
        for (target_ix, ek, irk) in candidates {
            let target_node = snapshot.node(target_ix);
            let target_id = target_node.symbol_id;

            let edge = match edge_index.get(&(target_id, ek)) {
                Some(e) => e,
                None => continue,
            };

            let range = edge
                .location
                .unwrap_or_else(|| fallback_range(subject_start_line));

            results.push(RelationEvidence {
                source_id: edge.source,
                target_id: edge.target,
                relation_kind: irk,
                file_id: subject_file_id.unwrap_or(target_node.file_id),
                range,
                confidence: edge.confidence,
            });
        }

        Ok(results)
    }

    fn count_incoming_by_kind(
        &self,
        symbol_id: &SymbolId,
    ) -> Result<HashMap<InternalRelationKind, usize>> {
        let snapshot = self.graph.snapshot();

        let mut counts = HashMap::new();
        for (_, ek) in snapshot.incoming_neighbors_with_kinds(symbol_id) {
            if let Some(irk) = InternalRelationKind::from_edge_kind(ek) {
                *counts.entry(irk).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }

    fn count_outgoing_by_kind(
        &self,
        symbol_id: &SymbolId,
    ) -> Result<HashMap<InternalRelationKind, usize>> {
        let snapshot = self.graph.snapshot();

        let mut counts = HashMap::new();
        for (_, ek) in snapshot.outgoing_neighbors_with_kinds(symbol_id) {
            if let Some(irk) = InternalRelationKind::from_edge_kind(ek) {
                *counts.entry(irk).or_insert(0) += 1;
            }
        }
        Ok(counts)
    }
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `InternalRelationKind::from_edge_kind` must map the expected set and
    /// return `None` for non-dossier edge kinds.
    #[test]
    fn edge_kind_to_internal_relation_kind() {
        // Dossier-supported kinds
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Calls).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::References).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Implements).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Extends).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Instantiates).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Reads).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Writes).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::FieldRead).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::FieldWrite).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Decorates).is_some());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::RegistersCallback).is_some());

        // Non-dossier kinds should map to None
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Contains).is_none());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Defines).is_none());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Includes).is_none());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Imports).is_none());
        assert!(InternalRelationKind::from_edge_kind(EdgeKind::Exports).is_none());
    }

    #[test]
    fn fallback_range_anchors_at_given_line() {
        let r = fallback_range(42);
        assert_eq!(r.start_line, 42);
        assert_eq!(r.end_line, 42);
        assert_eq!(r.start_byte, 0);
        assert_eq!(r.end_byte, 0);
        assert_eq!(r.start_column, 0);
        assert_eq!(r.end_column, 0);
    }
}
