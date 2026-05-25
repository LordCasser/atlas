//! GraphSnapshot: in-memory graph loaded from SQLite for fast traversal.
//!
//! Key design: graph queries do NOT hit SQLite; all data is pre-loaded into
//! HashMaps and adjacency lists. Snapshot is immutable after construction.

use db::Store;
use types::ids::{FileId, SymbolId};
use types::{
    Confidence, EdgeKind, Language, Provenance, RawEdge, SymbolDef, SymbolKind, Visibility,
};
use std::collections::{HashMap, VecDeque};

// ── type aliases ────────────────────────────────────────────────────────────

pub type NodeIx = usize;
pub type EdgeIx = usize;

// ── per-node summary ────────────────────────────────────────────────────────

/// Lightweight node for graph traversal. Strips extraneous SymbolDef fields.
#[derive(Debug, Clone)]
pub struct NodeSummary {
    pub symbol_id: SymbolId,
    pub kind: SymbolKind,
    pub name: String,
    pub qualified_name: String,
    pub file_id: FileId,
    pub language: Language,
    pub container: Option<SymbolId>,
    pub visibility: Option<Visibility>,
    /// Index into snapshot.edges for outgoing edge ids.
    pub outgoing: Vec<EdgeIx>,
    /// Index into snapshot.edges for incoming edge ids.
    pub incoming: Vec<EdgeIx>,
}

impl NodeSummary {
    fn from_symbol(sym: SymbolDef) -> Self {
        Self {
            symbol_id: sym.id,
            kind: sym.kind,
            name: sym.name,
            qualified_name: sym.qualified_name,
            file_id: sym.file_id,
            language: sym.language,
            container: sym.container,
            visibility: sym.visibility,
            outgoing: Vec::new(),
            incoming: Vec::new(),
        }
    }
}

// ── per-edge summary ────────────────────────────────────────────────────────

/// Lightweight edge for graph traversal.
#[derive(Debug, Clone)]
pub struct EdgeSummary {
    pub edge_id: types::ids::EdgeId,
    pub source: SymbolId,
    pub target: SymbolId,
    pub kind: EdgeKind,
    pub confidence: Confidence,
    pub provenance: Provenance,
    /// Index into snapshot.nodes for the source/target.
    pub source_ix: NodeIx,
    pub target_ix: NodeIx,
}

// ── snapshot ────────────────────────────────────────────────────────────────

/// Complete in-memory representation of the Atlas knowledge graph.
///
/// Immutable after construction. All queries use index-based lookups for O(1)
/// complexity.
#[derive(Clone)]
pub struct GraphSnapshot {
    pub nodes: Vec<NodeSummary>,
    pub edges: Vec<EdgeSummary>,

    // Primary index: SymbolId → position in nodes[]
    pub id_to_idx: HashMap<SymbolId, NodeIx>,

    // Multi-map indexes
    pub name_index: HashMap<String, Vec<NodeIx>>,
    pub qname_index: HashMap<String, Vec<NodeIx>>,
    pub file_index: HashMap<FileId, Vec<NodeIx>>,

    /// Total edge count (for stats).
    pub edge_count: usize,
}

impl GraphSnapshot {
    /// Load the full graph from the Store into memory.
    ///
    /// `confidence_threshold` (default 0.0) filters low-confidence edges.
    pub fn from_store(store: &Store, confidence_threshold: f32) -> anyhow::Result<Self> {
        let symbols = store.get_all_symbols()?;
        let edges = store.get_all_edges()?;
        Self::from_parts(symbols, edges, confidence_threshold)
    }

    /// Build from already-loaded vectors (useful for testing).
    pub fn from_parts(
        symbols: Vec<SymbolDef>,
        edges: Vec<RawEdge>,
        confidence_threshold: f32,
    ) -> anyhow::Result<Self> {
        // ── nodes ────────────────────────────────────────────────────────
        let mut nodes: Vec<NodeSummary> =
            symbols.into_iter().map(NodeSummary::from_symbol).collect();
        let mut id_to_idx: HashMap<SymbolId, usize> = HashMap::with_capacity(nodes.len());
        let mut name_index: HashMap<String, Vec<usize>> = HashMap::new();
        let mut qname_index: HashMap<String, Vec<usize>> = HashMap::new();
        let mut file_index: HashMap<FileId, Vec<usize>> = HashMap::new();

        for (ix, node) in nodes.iter().enumerate() {
            id_to_idx.insert(node.symbol_id, ix);
            name_index.entry(node.name.clone()).or_default().push(ix);
            qname_index
                .entry(node.qualified_name.clone())
                .or_default()
                .push(ix);
            file_index.entry(node.file_id).or_default().push(ix);
        }

        // ── edges (with confidence filter) ───────────────────────────────
        //
        // Note: edges whose source or target SymbolId is not in the symbols
        // vector are silently skipped. This intentionally applies to dataflow
        // edges (Parameter, Returns, Assigns, TypeOf, FieldRead, FieldWrite)
        // whose targets are virtual/ephemeral SymbolIds (language="dataflow")
        // that don't correspond to any defined symbol. Dataflow edges are
        // retained in SQLite for direct query but are excluded from the graph
        // because the graph models symbol-to-symbol relationships, not
        // variable-level data flow within functions.
        let mut edge_summaries: Vec<EdgeSummary> = Vec::with_capacity(edges.len());

        for e in edges {
            if e.confidence.as_f32() < confidence_threshold {
                continue;
            }
            let Some(&source_ix) = id_to_idx.get(&e.source) else {
                continue;
            };
            let Some(&target_ix) = id_to_idx.get(&e.target) else {
                continue;
            };
            edge_summaries.push(EdgeSummary {
                edge_id: e.id,
                source: e.source,
                target: e.target,
                kind: e.kind,
                confidence: e.confidence,
                provenance: e.provenance,
                source_ix,
                target_ix,
            });
        }

        let edge_count = edge_summaries.len();

        // Populate node adjacency lists
        for (eix, es) in edge_summaries.iter().enumerate() {
            nodes[es.source_ix].outgoing.push(eix);
            nodes[es.target_ix].incoming.push(eix);
        }

        Ok(Self {
            nodes,
            edges: edge_summaries,
            id_to_idx,
            name_index,
            qname_index,
            file_index,
            edge_count,
        })
    }

    // ── lookups ──────────────────────────────────────────────────────────

    #[inline]
    pub fn node(&self, ix: NodeIx) -> &NodeSummary {
        &self.nodes[ix]
    }

    #[inline]
    pub fn node_by_id(&self, id: &SymbolId) -> Option<&NodeSummary> {
        self.id_to_idx.get(id).map(|&ix| &self.nodes[ix])
    }

    #[inline]
    pub fn edge(&self, ix: EdgeIx) -> &EdgeSummary {
        &self.edges[ix]
    }

    /// Find nodes by name (case-sensitive exact).
    pub fn nodes_by_name(&self, name: &str) -> &[NodeIx] {
        self.name_index
            .get(name)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Find nodes by qualified name.
    pub fn nodes_by_qname(&self, qname: &str) -> &[NodeIx] {
        self.qname_index
            .get(qname)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Find all nodes in a given file.
    pub fn nodes_by_file(&self, file_id: &FileId) -> &[NodeIx] {
        self.file_index
            .get(file_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    // ── traversal helpers ────────────────────────────────────────────────

    /// Outgoing neighbors of a node, optionally filtered by edge kind.
    pub fn neighbors(
        &self,
        node_ix: NodeIx,
        direction: TraversalDirection,
        kind_filter: Option<&[EdgeKind]>,
    ) -> Vec<(NodeIx, EdgeIx)> {
        match direction {
            TraversalDirection::Outgoing => {
                self.filter_edges_outgoing(&self.nodes[node_ix].outgoing, node_ix, kind_filter)
            }
            TraversalDirection::Incoming => {
                self.filter_edges_incoming(&self.nodes[node_ix].incoming, kind_filter)
            }
            TraversalDirection::Both => {
                let mut both =
                    self.filter_edges_outgoing(&self.nodes[node_ix].outgoing, node_ix, kind_filter);
                both.extend(self.filter_edges_incoming(&self.nodes[node_ix].incoming, kind_filter));
                both
            }
        }
    }

    /// Incoming neighbors (sources that point to this node) with their edge kinds.
    pub fn incoming_neighbors_with_kinds(&self, id: &SymbolId) -> Vec<(NodeIx, EdgeKind)> {
        let Some(&node_ix) = self.id_to_idx.get(id) else {
            return vec![];
        };
        self.nodes[node_ix]
            .incoming
            .iter()
            .filter_map(|&eix| {
                let edge = &self.edges[eix];
                Some((edge.source_ix, edge.kind))
            })
            .collect()
    }

    /// Outgoing neighbors (targets this node points to) with their edge kinds.
    pub fn outgoing_neighbors_with_kinds(&self, id: &SymbolId) -> Vec<(NodeIx, EdgeKind)> {
        let Some(&node_ix) = self.id_to_idx.get(id) else {
            return vec![];
        };
        self.nodes[node_ix]
            .outgoing
            .iter()
            .filter_map(|&eix| {
                let edge = &self.edges[eix];
                Some((edge.target_ix, edge.kind))
            })
            .collect()
    }

    fn filter_edges_outgoing(
        &self,
        edge_indices: &[EdgeIx],
        _source_ix: NodeIx,
        kind_filter: Option<&[EdgeKind]>,
    ) -> Vec<(NodeIx, EdgeIx)> {
        edge_indices
            .iter()
            .filter_map(|&eix| {
                let edge = &self.edges[eix];
                if let Some(kinds) = kind_filter {
                    if !kinds.contains(&edge.kind) {
                        return None;
                    }
                }
                Some((edge.target_ix, eix))
            })
            .collect()
    }

    fn filter_edges_incoming(
        &self,
        edge_indices: &[EdgeIx],
        kind_filter: Option<&[EdgeKind]>,
    ) -> Vec<(NodeIx, EdgeIx)> {
        edge_indices
            .iter()
            .filter_map(|&eix| {
                let edge = &self.edges[eix];
                if let Some(kinds) = kind_filter {
                    if !kinds.contains(&edge.kind) {
                        return None;
                    }
                }
                Some((edge.source_ix, eix))
            })
            .collect()
    }

    /// BFS traversal from a set of starting nodes.
    pub fn bfs(
        &self,
        starts: &[NodeIx],
        config: &TraversalConfig,
    ) -> Vec<(NodeIx, usize /* depth */)> {
        let mut visited = vec![false; self.nodes.len()];
        let mut queue = VecDeque::new();
        let mut result = Vec::new();

        for &start in starts {
            if start < self.nodes.len() && !visited[start] {
                visited[start] = true;
                queue.push_back((start, 0));
            }
        }

        while let Some((current, depth)) = queue.pop_front() {
            if depth > config.max_depth {
                continue;
            }
            result.push((current, depth));
            if result.len() >= config.limit {
                break;
            }

            let neighbors = self.neighbors(
                current,
                config.direction,
                config.edge_kind_filter.as_deref(),
            );
            for (neighbor_ix, _) in neighbors {
                if !visited[neighbor_ix] {
                    visited[neighbor_ix] = true;
                    queue.push_back((neighbor_ix, depth + 1));
                }
            }
        }

        result
    }

    /// Shortest path between two nodes (BFS). Returns node indices in order.
    pub fn shortest_path(&self, from: NodeIx, to: NodeIx, max_depth: usize) -> Option<Vec<NodeIx>> {
        if from == to {
            return Some(vec![from]);
        }
        if from >= self.nodes.len() || to >= self.nodes.len() {
            return None;
        }

        let mut visited = vec![false; self.nodes.len()];
        let mut parent = vec![None; self.nodes.len()];
        let mut queue = VecDeque::new();

        visited[from] = true;
        queue.push_back((from, 0));

        while let Some((current, depth)) = queue.pop_front() {
            if current == to {
                // Reconstruct path
                let mut path = Vec::new();
                let mut node = to;
                loop {
                    path.push(node);
                    if node == from {
                        break;
                    }
                    node = parent[node].unwrap();
                }
                path.reverse();
                return Some(path);
            }

            if depth >= max_depth {
                continue;
            }

            // Follow all edges in both directions
            for edge_list in [&self.nodes[current].outgoing, &self.nodes[current].incoming] {
                for &eix in edge_list {
                    let edge = &self.edges[eix];
                    let neighbor = if edge.source_ix == current {
                        edge.target_ix
                    } else {
                        edge.source_ix
                    };
                    if !visited[neighbor] {
                        visited[neighbor] = true;
                        parent[neighbor] = Some(current);
                        queue.push_back((neighbor, depth + 1));
                    }
                }
            }
        }

        None
    }

    // ── stats ────────────────────────────────────────────────────────────

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }
}

// ── traversal config ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
pub enum TraversalDirection {
    Outgoing,
    Incoming,
    Both,
}

#[derive(Debug, Clone)]
pub struct TraversalConfig {
    pub direction: TraversalDirection,
    pub max_depth: usize,
    pub limit: usize,
    /// Optional edge kind filter. None = follow all kinds.
    pub edge_kind_filter: Option<Vec<EdgeKind>>,
}

impl Default for TraversalConfig {
    fn default() -> Self {
        Self {
            direction: TraversalDirection::Outgoing,
            max_depth: 3,
            limit: 100,
            edge_kind_filter: None,
        }
    }
}

// ── result types ────────────────────────────────────────────────────────────

/// A subgraph induced by a traversal.
#[derive(Debug, Clone, Default)]
pub struct Subgraph {
    pub node_indices: Vec<NodeIx>,
    pub edge_indices: Vec<EdgeIx>,
}

/// Call-graph focused view.
#[derive(Debug, Clone, Default)]
pub struct CallGraphView {
    pub callers: Vec<NodeIx>,
    pub callees: Vec<NodeIx>,
}

/// Path between two nodes.
#[derive(Debug, Clone)]
pub struct GraphPath {
    pub node_indices: Vec<NodeIx>,
    pub edge_indices: Vec<EdgeIx>,
}

// ── tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use types::ids::FileId;

    fn make_file_id(name: &str) -> FileId {
        FileId::generate(name)
    }

    fn make_symbol(file_id: FileId, name: &str, qname: &str, kind: SymbolKind) -> SymbolDef {
        let id = types::ids::SymbolId::generate(
            &file_id,
            "typescript",
            qname,
            kind.as_str(),
            None,
        );
        SymbolDef {
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

    fn make_edge(source: SymbolId, target: SymbolId, kind: EdgeKind) -> RawEdge {
        use types::ids::EdgeId;
        let id = EdgeId::generate(
            &source,
            &target,
            kind.as_str(),
            None,
            Provenance::TreeSitter.as_str(),
        );
        RawEdge::new(
            id,
            source,
            target,
            kind,
            Confidence::certain(),
            Provenance::TreeSitter,
        )
    }

    #[test]
    fn test_graph_snapshot_basic() {
        let fid = make_file_id("test.ts");
        let a = make_symbol(fid, "A", "A", SymbolKind::Class);
        let b = make_symbol(fid, "run", "A.run", SymbolKind::Method);
        let e = make_edge(a.id, b.id, EdgeKind::Contains);

        let snap = GraphSnapshot::from_parts(vec![a.clone(), b.clone()], vec![e], 0.0).unwrap();

        assert_eq!(snap.node_count(), 2);
        assert_eq!(snap.edge_count, 1);
        assert!(snap.node_by_id(&a.id).is_some());
        assert_eq!(snap.nodes_by_name("A").len(), 1);
    }

    #[test]
    fn test_graph_snapshot_neighbors() {
        let fid = make_file_id("test.ts");
        let caller = make_symbol(fid, "main", "main", SymbolKind::Function);
        let callee = make_symbol(fid, "helper", "helper", SymbolKind::Function);
        let e = make_edge(caller.id, callee.id, EdgeKind::Calls);

        let snap =
            GraphSnapshot::from_parts(vec![caller.clone(), callee.clone()], vec![e], 0.0).unwrap();

        let caller_ix = snap.id_to_idx[&caller.id];
        let neighbors = snap.neighbors(caller_ix, TraversalDirection::Outgoing, None);
        assert_eq!(neighbors.len(), 1);
        assert_eq!(snap.node(neighbors[0].0).symbol_id, callee.id);
    }

    #[test]
    fn test_graph_snapshot_bfs() {
        let fid = make_file_id("test.ts");
        let a = make_symbol(fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(fid, "b", "b", SymbolKind::Function);
        let c = make_symbol(fid, "c", "c", SymbolKind::Function);
        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);
        let e2 = make_edge(b.id, c.id, EdgeKind::Calls);

        let snap =
            GraphSnapshot::from_parts(vec![a.clone(), b.clone(), c.clone()], vec![e1, e2], 0.0)
                .unwrap();

        let a_ix = snap.id_to_idx[&a.id];
        let config = TraversalConfig {
            direction: TraversalDirection::Outgoing,
            max_depth: 2,
            limit: 100,
            edge_kind_filter: Some(vec![EdgeKind::Calls]),
        };
        let result = snap.bfs(&[a_ix], &config);
        assert_eq!(result.len(), 3); // a, b, c
    }

    #[test]
    fn test_graph_shortest_path() {
        let fid = make_file_id("test.ts");
        let a = make_symbol(fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(fid, "b", "b", SymbolKind::Function);
        let c = make_symbol(fid, "c", "c", SymbolKind::Function);
        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);
        let e2 = make_edge(b.id, c.id, EdgeKind::Calls);

        let snap =
            GraphSnapshot::from_parts(vec![a.clone(), b.clone(), c.clone()], vec![e1, e2], 0.0)
                .unwrap();

        let a_ix = snap.id_to_idx[&a.id];
        let c_ix = snap.id_to_idx[&c.id];
        let path = snap.shortest_path(a_ix, c_ix, 5).unwrap();
        assert_eq!(path.len(), 3);
    }

    #[test]
    fn test_confidence_filter() {
        let fid = make_file_id("test.ts");
        let caller = make_symbol(fid, "main", "main", SymbolKind::Function);
        let callee = make_symbol(fid, "low", "low", SymbolKind::Function);
        let mut e = make_edge(caller.id, callee.id, EdgeKind::Calls);
        e.confidence = Confidence::new(0.3);

        let snap = GraphSnapshot::from_parts(
            vec![caller.clone(), callee.clone()],
            vec![e],
            0.5, // threshold > confidence
        )
        .unwrap();

        assert_eq!(snap.edge_count, 0);
    }
}
