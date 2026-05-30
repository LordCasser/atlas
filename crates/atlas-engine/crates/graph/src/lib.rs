//! Graph layer: in-memory GraphSnapshot with BFS/DFS traversal,
//! call graph analysis, import analysis, impact radius, shortest path.
//!
//! P2: GraphBuilder is separated from ReferenceResolver — the resolver
//! only produces resolved facts, GraphBuilder converts them to edges.

pub mod annotation_graph;
pub mod graph_builder;
pub mod snapshot;

use std::sync::Arc;

use types::EdgeKind;
use types::ids::SymbolId;

pub use annotation_graph::materialize_annotations;
pub use graph_builder::{GraphBuilder, GraphBuilderStats};
pub use snapshot::{
    CallGraphView, ForwardFrontier, FrontierNode, GraphPath, GraphSnapshot, NodeIx, NodeSummary,
    CompositePathScore, RankedPath,
    PathBreakpoint, PathBreakpointKind, PathEdge, PathEdgeDirection, Subgraph, TraversalConfig,
    TraversalDirection,
};

use db::Store;

/// High-level graph query engine backed by an immutable `GraphSnapshot`.
///
/// All queries are O(1) lookups or bounded BFS/DFS traversals — no SQLite round-trips.
pub struct GraphEngine {
    snapshot: Arc<GraphSnapshot>,
}

impl GraphEngine {
    /// Build a GraphEngine by loading the full graph from the Store.
    ///
    /// `confidence_threshold` filters out low-confidence edges (0.0 = keep all).
    pub fn from_store(store: &Store, confidence_threshold: f32) -> anyhow::Result<Self> {
        let snapshot = GraphSnapshot::from_store(store, confidence_threshold)?;
        Ok(Self {
            snapshot: Arc::new(snapshot),
        })
    }

    /// Build a scoped GraphEngine containing only symbols and edges for
    /// the given files. Faster than full rebuild when only a few files changed.
    ///
    /// Reserved for future delta graph merge implementation.
    #[allow(dead_code)]
    pub fn from_files(
        store: &Store,
        file_ids: &[types::ids::FileId],
        confidence_threshold: f32,
    ) -> anyhow::Result<Self> {
        let snapshot = GraphSnapshot::from_files(store, file_ids, confidence_threshold)?;
        Ok(Self {
            snapshot: Arc::new(snapshot),
        })
    }

    /// Build from an already-constructed snapshot (for testing).
    pub fn from_snapshot(snapshot: GraphSnapshot) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
        }
    }

    /// Access the underlying snapshot.
    pub fn snapshot(&self) -> &GraphSnapshot {
        &self.snapshot
    }

    // ── basic lookups ────────────────────────────────────────────────────

    pub fn node_count(&self) -> usize {
        self.snapshot.node_count()
    }

    pub fn edge_count(&self) -> usize {
        self.snapshot.edge_count
    }

    /// Total degree (in + out) for a symbol. 0 if not found.
    pub fn degree(&self, id: &SymbolId) -> usize {
        self.snapshot
            .id_to_idx
            .get(id)
            .map(|&ix| {
                let node = &self.snapshot.nodes[ix];
                node.outgoing.len() + node.incoming.len()
            })
            .unwrap_or(0)
    }

    /// Resolve a slice of NodeIx indices to SymbolIds.
    pub fn resolve_node_ids(&self, indices: &[NodeIx]) -> Vec<SymbolId> {
        indices
            .iter()
            .filter_map(|&ix| self.snapshot.nodes.get(ix).map(|n| n.symbol_id))
            .collect()
    }

    // ── neighbors ────────────────────────────────────────────────────────

    /// Direct neighbors of a symbol, optionally filtered by edge kinds.
    /// Supports multi-hop traversal via config.max_depth.
    pub fn neighbors(&self, id: &SymbolId, config: TraversalConfig) -> Subgraph {
        let Some(&start_ix) = self.snapshot.id_to_idx.get(id) else {
            return Subgraph::default();
        };

        let max_depth = config.max_depth.max(1).min(5);
        let mut visited_nodes = std::collections::HashSet::new();
        let mut visited_edges = std::collections::HashSet::new();
        let mut frontier = vec![start_ix];
        // Note: start_ix is NOT added to visited_nodes — callers/callees
        // should not include the queried symbol in results.

        for _depth in 0..max_depth {
            let mut next_frontier = Vec::new();
            for &node_ix in &frontier {
                let pairs = self.snapshot.neighbors(
                    node_ix,
                    config.direction,
                    config.edge_kind_filter.as_deref(),
                );
                for (nix, eix) in pairs {
                    if visited_nodes.insert(nix) {
                        next_frontier.push(nix);
                    }
                    visited_edges.insert(eix);
                }
            }
            frontier = next_frontier;
            if frontier.is_empty() {
                break;
            }
        }

        let mut sub = Subgraph::default();
        sub.node_indices = visited_nodes.into_iter().collect();
        sub.edge_indices = visited_edges.into_iter().collect();
        sub
    }

    /// Kinds of edges that represent "calls" — includes promoted constructor/interface edges.
    const CALL_EDGES: &[EdgeKind] = &[
        EdgeKind::Calls,
        EdgeKind::Instantiates,
        EdgeKind::Implements,
    ];

    /// Default edge kinds for path-finding (calls + dynamic dispatch boundaries).
    /// Excludes non-control-flow edges (References, TypeOf, Contains, etc.)
    /// to avoid semantically meaningless paths in security analysis.
    pub const PATH_EDGES: &[EdgeKind] = &[
        EdgeKind::Calls,
        EdgeKind::Instantiates,
        EdgeKind::Implements,
        EdgeKind::RegistersCallback,
    ];

    // ── callers / callees / callgraph ────────────────────────────────────

    /// Find direct callers (incoming Calls + promoted Instantiates/Implements edges).
    pub fn callers(&self, id: &SymbolId) -> CallGraphView {
        let config = TraversalConfig {
            direction: TraversalDirection::Incoming,
            max_depth: 1,
            limit: 200,
            edge_kind_filter: Some(Self::CALL_EDGES.to_vec()),
        };
        let sub = self.neighbors(id, config);
        CallGraphView {
            callers: sub.node_indices,
            callees: vec![],
        }
    }

    /// Find direct callees (outgoing Calls + promoted Instantiates/Implements edges).
    pub fn callees(&self, id: &SymbolId) -> CallGraphView {
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            max_depth: 1,
            limit: 200,
            edge_kind_filter: Some(Self::CALL_EDGES.to_vec()),
        };
        let sub = self.neighbors(id, config);
        CallGraphView {
            callers: vec![],
            callees: sub.node_indices,
        }
    }

    /// Full call graph around a symbol (both directions, configurable depth).
    pub fn callgraph(&self, id: &SymbolId, depth: usize) -> Subgraph {
        let Some(&start) = self.snapshot.id_to_idx.get(id) else {
            return Subgraph::default();
        };
        let config = TraversalConfig {
            direction: TraversalDirection::Both,
            max_depth: depth,
            limit: 500,
            edge_kind_filter: Some(Self::CALL_EDGES.to_vec()),
        };
        let visited = self.snapshot.bfs(&[start], &config);
        Subgraph {
            node_indices: visited.iter().map(|(nix, _)| *nix).collect(),
            edge_indices: vec![], // edges would require full revisit
        }
    }

    // ── import dependencies ──────────────────────────────────────────────

    /// Find all imports (incoming Imports edges → who imports this file/module).
    pub fn importers(&self, file_id: &types::ids::FileId) -> Vec<SymbolId> {
        let node_ixs = self.snapshot.nodes_by_file(file_id);
        let mut importers = std::collections::HashSet::new();
        for &nix in node_ixs {
            for &eix in &self.snapshot.nodes[nix].incoming {
                let edge = self.snapshot.edge(eix);
                if edge.kind == EdgeKind::Imports {
                    importers.insert(edge.source);
                }
            }
        }
        importers.into_iter().collect()
    }

    /// Find all exports (outgoing Exports edges).
    pub fn dependencies(&self, file_id: &types::ids::FileId) -> Vec<types::ids::FileId> {
        let node_ixs = self.snapshot.nodes_by_file(file_id);
        let mut deps = std::collections::HashSet::new();
        for &nix in node_ixs {
            for &eix in &self.snapshot.nodes[nix].outgoing {
                let edge = self.snapshot.edge(eix);
                if edge.kind == EdgeKind::Imports || edge.kind == EdgeKind::Includes {
                    if let Some(dep_node) = self.snapshot.node_by_id(&edge.target) {
                        deps.insert(dep_node.file_id);
                    }
                }
            }
        }
        deps.into_iter().collect()
    }

    // ── impact analysis ──────────────────────────────────────────────────

    /// Impact radius: BFS bidirectionally (follows Calls + Imports) up to `depth`.
    /// Both downstream (what this affects) and upstream (what affects this)
    /// are traversed. Only call and import edges are traversed; type references
    /// (References) and container edges (Contains) are excluded to avoid noise
    /// from struct fields, local variables, and type aliases.
    pub fn impact(&self, id: &SymbolId, depth: usize) -> Subgraph {
        let Some(&start) = self.snapshot.id_to_idx.get(id) else {
            return Subgraph::default();
        };
        let config = TraversalConfig {
            direction: TraversalDirection::Both,
            max_depth: depth,
            limit: 1000,
            edge_kind_filter: Some(vec![
                EdgeKind::Calls,
                EdgeKind::Imports,
            ]),
        };
        let visited = self.snapshot.bfs(&[start], &config);
        Subgraph {
            node_indices: visited.iter().map(|(nix, _)| *nix).collect(),
            edge_indices: vec![],
        }
    }

    /// Impact radius including container children.
    /// Expands the starting set by adding all symbols whose `container` is the target,
    /// then runs bidirectional BFS.
    pub fn impact_with_children(&self, id: &SymbolId, depth: usize) -> Subgraph {
        let Some(&start) = self.snapshot.id_to_idx.get(id) else {
            return Subgraph::default();
        };
        // Collect children: all nodes whose container == target
        let mut starts: Vec<NodeIx> = vec![start];
        for (ix, node) in self.snapshot.nodes.iter().enumerate() {
            if node.container.as_ref() == Some(id) {
                starts.push(ix);
            }
        }
        let config = TraversalConfig {
            direction: TraversalDirection::Both,
            max_depth: depth,
            limit: 1000,
            edge_kind_filter: Some(vec![
                EdgeKind::Calls,
                EdgeKind::Imports,
            ]),
        };
        let visited = self.snapshot.bfs(&starts, &config);
        Subgraph {
            node_indices: visited.iter().map(|(nix, _)| *nix).collect(),
            edge_indices: vec![],
        }
    }

    // ── shortest path ────────────────────────────────────────────────────

    /// Find the shortest path between two symbols.
    ///
    /// When `edge_kind_filter` is `Some(kinds)`, only edges of the given kinds are
    /// traversed. `None` follows all edge kinds (backward-compatible).
    ///
    /// `direction` controls which edges are followed during BFS:
    /// - `Outgoing`: only forward edges (source→target)
    /// - `Incoming`: only reverse edges (target→source)
    /// - `Both`: bidirectional (default for finding any path)
    ///
    /// When `prefer_production` is true, the BFS prefers paths through non-test
    /// files. Test file nodes are deferred to a secondary exploration queue,
    /// guaranteeing that if a pure production path exists, it will be returned
    /// even if a shorter (by hop count) path through test code also exists.
    pub fn shortest_path(
        &self,
        from: &SymbolId,
        to: &SymbolId,
        max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>,
        direction: TraversalDirection,
        prefer_production: bool,
    ) -> Option<GraphPath> {
        let from_ix = self.snapshot.id_to_idx.get(from)?;
        let to_ix = self.snapshot.id_to_idx.get(to)?;
        self.snapshot
            .shortest_path(*from_ix, *to_ix, max_depth, edge_kind_filter, direction, prefer_production)
    }

    /// Walk forward from `start_symbols` to find the deepest reachable
    /// nodes via outgoing call edges.  When the forward walk exhausts, the
    /// returned [`ForwardFrontier`] describes **where** static-analysis
    /// could not proceed further — typically dynamic dispatch boundaries
    /// (function pointers, virtual calls).
    ///
    /// Call this after `shortest_path` returns `None` with `Outgoing`
    /// direction to produce a diagnostic for the user.
    pub fn forward_frontier(
        &self,
        start_symbols: &[SymbolId],
        max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>,
    ) -> ForwardFrontier {
        self.snapshot.forward_frontier(start_symbols, max_depth, edge_kind_filter)
    }

    // ── usages ───────────────────────────────────────────────────────────

    /// Find all references that point to this symbol.
    /// NOTE: returns reference target indices; actual `ReferenceUse` data is in the Store.
    pub fn usage_symbols(&self, id: &SymbolId) -> Vec<SymbolId> {
        let Some(&target_ix) = self.snapshot.id_to_idx.get(id) else {
            return vec![];
        };
        let mut users = Vec::new();
        for &eix in &self.snapshot.nodes[target_ix].incoming {
            let edge = self.snapshot.edge(eix);
            users.push(edge.source);
        }
        users
    }

    // ── file-level ───────────────────────────────────────────────────────

    /// Get all symbols defined in a file.
    pub fn file_symbols(&self, file_id: &types::ids::FileId) -> Vec<SymbolId> {
        self.snapshot
            .nodes_by_file(file_id)
            .iter()
            .map(|&ix| self.snapshot.nodes[ix].symbol_id)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::{FileId, SymbolId};
    use types::{Confidence, Language, Provenance, SymbolKind, Visibility};

    fn make_file_id(name: &str) -> FileId {
        FileId::generate(name)
    }

    fn make_symbol(file_id: FileId, name: &str, qname: &str, kind: SymbolKind) -> types::SymbolDef {
        let id = SymbolId::generate(&file_id, "typescript", qname, kind.as_str(), None);
        types::SymbolDef {
            id,
            kind,
            name: name.to_string(),
            qualified_name: qname.to_string(),
            symbol_path: vec![],
            file_id,
            language: Language::TypeScript,
            range: Default::default(),
            name_range: Default::default(),
            signature: None,
            visibility: Some(Visibility::Public),
            exported: false,
            static_: false,
            async_: false,
            container: None,
            scope_id: None,
            package_name: None,
            namespace_path: vec![],
            layer: "structural".to_string(),
        }
    }

    fn make_edge(source: SymbolId, target: SymbolId, kind: EdgeKind) -> types::RawEdge {
        use types::ids::EdgeId;
        let id = EdgeId::generate(
            &source,
            &target,
            kind.as_str(),
            None,
            Provenance::TreeSitter.as_str(),
        );
        types::RawEdge::new(
            id,
            source,
            target,
            kind,
            Confidence::certain(),
            Provenance::TreeSitter,
        )
    }

    fn test_snapshot() -> GraphSnapshot {
        let fid = make_file_id("test.ts");
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let b = make_symbol(fid, "helper", "helper", SymbolKind::Function);
        let c = make_symbol(fid, "log", "log", SymbolKind::Function);
        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);
        let e2 = make_edge(b.id, c.id, EdgeKind::Calls);
        GraphSnapshot::from_parts(vec![a, b, c], vec![e1, e2], 0.0).unwrap()
    }

    #[test]
    fn test_engine_callers_callees() {
        let engine = GraphEngine::from_snapshot(test_snapshot());

        // b's caller is main (a)
        let fid = make_file_id("test.ts");
        let b = make_symbol(fid, "helper", "helper", SymbolKind::Function);
        let callers = engine.callers(&b.id);
        assert_eq!(callers.callers.len(), 1);

        // main's callee is helper (b)
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let callees = engine.callees(&a.id);
        assert_eq!(callees.callees.len(), 1);
    }

    #[test]
    fn test_engine_callgraph() {
        let engine = GraphEngine::from_snapshot(test_snapshot());
        let fid = make_file_id("test.ts");
        let b = make_symbol(fid, "helper", "helper", SymbolKind::Function);
        let cg = engine.callgraph(&b.id, 2);
        assert!(cg.node_indices.len() >= 3); // self + upstream + downstream
    }

    #[test]
    fn test_engine_shortest_path() {
        let engine = GraphEngine::from_snapshot(test_snapshot());
        let fid = make_file_id("test.ts");
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let c = make_symbol(fid, "log", "log", SymbolKind::Function);
        let path = engine.shortest_path(&a.id, &c.id, 5, None, TraversalDirection::Both, false);
        assert!(path.is_some());
    }

    #[test]
    fn test_engine_usage_symbols() {
        let engine = GraphEngine::from_snapshot(test_snapshot());
        let fid = make_file_id("test.ts");
        let c = make_symbol(fid, "log", "log", SymbolKind::Function);
        let users = engine.usage_symbols(&c.id);
        assert_eq!(users.len(), 1); // only helper calls log
    }
}
