//! GraphSnapshot: in-memory graph loaded from SQLite for fast traversal.
//!
//! Key design: graph queries do NOT hit SQLite; all data is pre-loaded into
//! HashMaps and adjacency lists. Snapshot is immutable after construction.

use db::Store;
use std::collections::{HashMap, VecDeque};
use types::ids::{FileId, SymbolId};
use types::{
    Confidence, EdgeKind, Language, Provenance, RawEdge, SymbolDef, SymbolKind, Visibility,
};

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
    pub start_line: u32,
    pub language: Language,
    pub is_test_file: bool,
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
            start_line: sym.range.start_line,
            language: sym.language,
            is_test_file: false, // set later during snapshot construction
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

// ── test file detection ─────────────────────────────────────────────────────

/// Check whether a file path (relative to project root) is likely a test file.
///
/// Uses both directory-based and name-based heuristics:
/// - Files under `tests/`, `test/`, `spec/`, `__tests__/` directories
/// - Files named `*_test.*`, `*.test.*`, `*.spec.*`, `*_spec.*`
/// - Files named `test_*` or `*_test` (prefix/suffix patterns)
///
/// This is intentionally broad — false positives only affect path quality
/// scoring, not correctness.
fn is_likely_test_path(path: &str) -> bool {
    let lower = path.to_lowercase();

    // Directory-based: files inside test/spec directories
    let dir_markers = ["/test/", "/tests/", "/spec/", "/__tests__/", "/testing/", "test/", "tests/"];
    for marker in &dir_markers {
        if lower.starts_with(marker) || lower.contains(marker) {
            return true;
        }
    }

    // Name-based: common test file naming patterns
    // Extract the filename stem (before extension)
    let file_stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    if file_stem.ends_with("_test")
        || file_stem.ends_with(".test")
        || file_stem.ends_with(".spec")
        || file_stem.ends_with("_spec")
        || file_stem.starts_with("test_")
        || file_stem.starts_with("test")
        || file_stem == "test"
    {
        return true;
    }

    // Name-based: filename contains "test" or "spec"
    let file_name = std::path::Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();

    if file_name.contains(".test.") || file_name.contains(".spec.") {
        return true;
    }

    false
}

impl GraphSnapshot {
    /// Load the full graph from the Store into memory.
    ///
    /// `confidence_threshold` (default 0.0) filters low-confidence edges.
    pub fn from_store(store: &Store, confidence_threshold: f32) -> anyhow::Result<Self> {
        let symbols = store.get_all_symbols()?;
        let edges = store.get_all_edges()?;
        // Build a FileId → file path map for test file detection.
        let file_paths: HashMap<FileId, String> = store
            .list_files()
            .unwrap_or_default()
            .into_iter()
            .map(|f| (f.file_id, f.path))
            .collect();
        Self::from_parts_with_paths(symbols, edges, confidence_threshold, &file_paths)
    }

    /// Build from already-loaded vectors (useful for testing).
    /// All files are treated as non-test (is_test_file = false).
    pub fn from_parts(
        symbols: Vec<SymbolDef>,
        edges: Vec<RawEdge>,
        confidence_threshold: f32,
    ) -> anyhow::Result<Self> {
        let empty_paths = HashMap::new();
        Self::from_parts_with_paths(symbols, edges, confidence_threshold, &empty_paths)
    }

    /// Build from already-loaded vectors with file path information for
    /// test file detection.
    pub fn from_parts_with_paths(
        symbols: Vec<SymbolDef>,
        edges: Vec<RawEdge>,
        confidence_threshold: f32,
        file_paths: &HashMap<FileId, String>,
    ) -> anyhow::Result<Self> {
        // ── nodes ────────────────────────────────────────────────────────
        let mut nodes: Vec<NodeSummary> =
            symbols.into_iter().map(NodeSummary::from_symbol).collect();
        let mut id_to_idx: HashMap<SymbolId, usize> = HashMap::with_capacity(nodes.len());
        let mut name_index: HashMap<String, Vec<usize>> = HashMap::new();
        let mut qname_index: HashMap<String, Vec<usize>> = HashMap::new();
        let mut file_index: HashMap<FileId, Vec<usize>> = HashMap::new();

        // Resolve test file status for each node based on its file path.
        for node in nodes.iter_mut() {
            if let Some(path) = file_paths.get(&node.file_id) {
                node.is_test_file = is_likely_test_path(path);
            }
        }

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

    /// Shortest path between two nodes (BFS). Returns enhanced path with
    /// direction, confidence, and breakpoint metadata.
    ///
    /// When `edge_kind_filter` is `Some(kinds)`, only edges of the given kinds
    /// are traversed. `None` follows all edge kinds (backward-compatible).
    ///
    /// When `direction` is `Outgoing`, only forward edges (source→target) are
    /// followed during BFS. `Incoming` follows only reverse edges (target→source).
    /// `Both` follows all edges bidirectionally (the default for finding any path).
    ///
    /// When `prefer_production` is true, production-code paths are preferred
    /// over paths that pass through test files. The BFS uses a two-queue
    /// approach: production nodes are explored first, test-file nodes are
    /// deferred to a secondary frontier. This guarantees that if a pure
    /// production path exists within max_depth, it will be returned instead
    /// of a shorter (by hop count) test-contaminated path.
    pub fn shortest_path(
        &self,
        from: NodeIx,
        to: NodeIx,
        max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>,
        direction: TraversalDirection,
        prefer_production: bool,
    ) -> Option<GraphPath> {
        if from == to {
            return Some(GraphPath {
                node_indices: vec![from],
                edges: vec![],
                confidence: 1.0,
                breakpoints: vec![],
                edge_indices: vec![],
            });
        }
        if from >= self.nodes.len() || to >= self.nodes.len() {
            return None;
        }

        if prefer_production {
            return self.shortest_path_prefer_production(from, to, max_depth, edge_kind_filter, direction);
        }
        self.shortest_path_bfs(from, to, max_depth, edge_kind_filter, direction)
    }

    /// Standard bidirectional BFS — unchanged behavior, enhanced output.
    fn shortest_path_bfs(
        &self,
        from: NodeIx,
        to: NodeIx,
        max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>,
        direction: TraversalDirection,
    ) -> Option<GraphPath> {
        let mut visited = vec![false; self.nodes.len()];
        let mut parent = vec![None; self.nodes.len()];
        let mut parent_edge = vec![None; self.nodes.len()];
        let mut queue = VecDeque::new();

        visited[from] = true;
        queue.push_back((from, 0));

        while let Some((current, depth)) = queue.pop_front() {
            if current == to {
                return Some(self.reconstruct_path(from, to, &parent_edge, &parent));
            }

            if depth >= max_depth {
                continue;
            }

            // Build edge lists based on direction constraint
            let edge_lists: Vec<&Vec<EdgeIx>> = match direction {
                TraversalDirection::Outgoing => vec![&self.nodes[current].outgoing],
                TraversalDirection::Incoming => vec![&self.nodes[current].incoming],
                TraversalDirection::Both => vec![&self.nodes[current].outgoing, &self.nodes[current].incoming],
            };

            for edge_list in &edge_lists {
                for &eix in *edge_list {
                    let edge = &self.edges[eix];
                    // Apply edge kind filter
                    if let Some(kinds) = edge_kind_filter {
                        if !kinds.contains(&edge.kind) {
                            continue;
                        }
                    }
                    let neighbor = if edge.source_ix == current {
                        edge.target_ix
                    } else {
                        edge.source_ix
                    };
                    if !visited[neighbor] {
                        visited[neighbor] = true;
                        parent[neighbor] = Some(current);
                        parent_edge[neighbor] = Some(eix);
                        queue.push_back((neighbor, depth + 1));
                    }
                }
            }
        }

        None
    }

    /// Production-preferring BFS using a two-queue approach (0-1 BFS).
    /// Nodes in test files have their exploration deferred behind all
    /// production nodes at the same depth.
    fn shortest_path_prefer_production(
        &self,
        from: NodeIx,
        to: NodeIx,
        max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>,
        direction: TraversalDirection,
    ) -> Option<GraphPath> {
        let mut visited = vec![false; self.nodes.len()];
        let mut parent = vec![None; self.nodes.len()];
        let mut parent_edge = vec![None; self.nodes.len()];

        // Two queues: primary (production) and secondary (test files)
        let mut primary_queue: VecDeque<(NodeIx, usize)> = VecDeque::new();
        let mut secondary_queue: VecDeque<(NodeIx, usize)> = VecDeque::new();

        visited[from] = true;
        // Push to the appropriate queue based on whether 'from' is test code
        if self.nodes[from].is_test_file {
            secondary_queue.push_back((from, 0));
        } else {
            primary_queue.push_back((from, 0));
        }

        loop {
            // Always drain the primary queue first (production paths)
            let next = primary_queue.pop_front().or_else(|| secondary_queue.pop_front());
            let (current, depth) = match next {
                Some(x) => x,
                None => break,
            };

            if current == to {
                return Some(self.reconstruct_path(from, to, &parent_edge, &parent));
            }

            if depth >= max_depth {
                continue;
            }

            let edge_lists: Vec<&Vec<EdgeIx>> = match direction {
                TraversalDirection::Outgoing => vec![&self.nodes[current].outgoing],
                TraversalDirection::Incoming => vec![&self.nodes[current].incoming],
                TraversalDirection::Both => vec![&self.nodes[current].outgoing, &self.nodes[current].incoming],
            };

            for edge_list in &edge_lists {
                for &eix in *edge_list {
                    let edge = &self.edges[eix];
                    if let Some(kinds) = edge_kind_filter {
                        if !kinds.contains(&edge.kind) {
                            continue;
                        }
                    }
                    let neighbor = if edge.source_ix == current {
                        edge.target_ix
                    } else {
                        edge.source_ix
                    };
                    if !visited[neighbor] {
                        visited[neighbor] = true;
                        parent[neighbor] = Some(current);
                        parent_edge[neighbor] = Some(eix);
                        // Route to primary or secondary queue based on file type
                        if self.nodes[neighbor].is_test_file {
                            secondary_queue.push_back((neighbor, depth + 1));
                        } else {
                            primary_queue.push_back((neighbor, depth + 1));
                        }
                    }
                }
            }
        }

        None
    }

    /// Reconstruct a GraphPath from BFS parent pointers.
    /// Walks from `to` backward to `from`, then reverses and computes
    /// direction, confidence, and breakpoints.
    fn reconstruct_path(
        &self,
        from: NodeIx,
        to: NodeIx,
        parent_edge: &[Option<EdgeIx>],
        parent: &[Option<NodeIx>],
    ) -> GraphPath {
        let mut path_nodes = Vec::new();
        let mut raw_edges = Vec::new();
        let mut node = to;
        loop {
            path_nodes.push(node);
            if node == from {
                break;
            }
            raw_edges.push(parent_edge[node].unwrap());
            node = parent[node].unwrap();
        }
        path_nodes.reverse();
        raw_edges.reverse();

        // Compute per-edge direction and aggregate confidence
        let mut edges: Vec<PathEdge> = Vec::with_capacity(raw_edges.len());
        let mut total_confidence = 0.0;
        let mut breakpoints: Vec<PathBreakpoint> = Vec::new();

        for (i, &eix) in raw_edges.iter().enumerate() {
            let edge = &self.edges[eix];
            let current_node = path_nodes[i];
            let next_node = path_nodes[i + 1];

            let direction = if edge.source_ix == current_node && edge.target_ix == next_node {
                PathEdgeDirection::Forward
            } else if edge.source_ix == next_node && edge.target_ix == current_node {
                PathEdgeDirection::Reverse
            } else {
                // Should not happen in a valid path, but handle gracefully
                PathEdgeDirection::Forward
            };

            total_confidence += edge.confidence.as_f32() as f64;

            // Detect breakpoints: low-confidence edges and callback boundaries
            let conf = edge.confidence.as_f32() as f64;
            if conf < 0.5 {
                breakpoints.push(PathBreakpoint {
                    kind: PathBreakpointKind::LowConfidence,
                    edge_index: i,
                    from_node: self.nodes[current_node].qualified_name.clone(),
                    to_node: self.nodes[next_node].qualified_name.clone(),
                    message: format!(
                        "Low-confidence edge ({:.2}): {} → {}",
                        conf,
                        self.nodes[current_node].name,
                        self.nodes[next_node].name,
                    ),
                });
            }

            if edge.kind == EdgeKind::RegistersCallback {
                breakpoints.push(PathBreakpoint {
                    kind: PathBreakpointKind::CallbackRegistration,
                    edge_index: i,
                    from_node: self.nodes[current_node].qualified_name.clone(),
                    to_node: self.nodes[next_node].qualified_name.clone(),
                    message: format!(
                        "Callback registration boundary: {} registers {}",
                        self.nodes[current_node].name,
                        self.nodes[next_node].name,
                    ),
                });
            }

            // Check for direction reversals that indicate indirect paths
            if direction == PathEdgeDirection::Reverse {
                breakpoints.push(PathBreakpoint {
                    kind: PathBreakpointKind::ReversedEdge,
                    edge_index: i,
                    from_node: self.nodes[current_node].qualified_name.clone(),
                    to_node: self.nodes[next_node].qualified_name.clone(),
                    message: format!(
                        "Edge traversed in reverse: {} is called by {} (not the other way)",
                        self.nodes[current_node].name,
                        self.nodes[next_node].name,
                    ),
                });
            }

            // Check for indirect dispatch edges (virtual calls via Instantiates/Implements)
            if edge.kind == EdgeKind::Instantiates || edge.kind == EdgeKind::Implements {
                breakpoints.push(PathBreakpoint {
                    kind: PathBreakpointKind::IndirectCall,
                    edge_index: i,
                    from_node: self.nodes[current_node].qualified_name.clone(),
                    to_node: self.nodes[next_node].qualified_name.clone(),
                    message: format!(
                        "Indirect dispatch: {} {} {}",
                        self.nodes[current_node].name,
                        edge.kind.as_str(),
                        self.nodes[next_node].name,
                    ),
                });
            }

            // Check for test file contamination on the target node
            if self.nodes[next_node].is_test_file {
                breakpoints.push(PathBreakpoint {
                    kind: PathBreakpointKind::TestContamination,
                    edge_index: i,
                    from_node: self.nodes[current_node].qualified_name.clone(),
                    to_node: self.nodes[next_node].qualified_name.clone(),
                    message: format!(
                        "Path enters test file node: {}",
                        self.nodes[next_node].qualified_name,
                    ),
                });
            }

            edges.push(PathEdge {
                edge_ix: eix,
                direction,
            });
        }

        let confidence = if edges.is_empty() {
            1.0
        } else {
            total_confidence / edges.len() as f64
        };

        GraphPath {
            node_indices: path_nodes,
            edge_indices: raw_edges,
            edges,
            confidence,
            breakpoints,
        }
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

/// Direction of a single edge as traversed in a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathEdgeDirection {
    /// Traversed in the edge's stored direction (source → target).
    Forward,
    /// Traversed opposite to the edge's stored direction (target → source).
    Reverse,
}

impl PathEdgeDirection {
    pub fn as_str(&self) -> &'static str {
        match self {
            PathEdgeDirection::Forward => "forward",
            PathEdgeDirection::Reverse => "reverse",
        }
    }
}

/// A single edge hop in a path with traversal direction.
#[derive(Debug, Clone)]
pub struct PathEdge {
    pub edge_ix: EdgeIx,
    pub direction: PathEdgeDirection,
}

/// Classification of a path breakpoint (indirection, gap, or low-quality edge).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathBreakpointKind {
    /// Edge confidence < 0.5 (function pointer resolution, heuristic match).
    LowConfidence,
    /// Edge is a RegistersCallback — traversal stops at callback boundaries.
    CallbackRegistration,
    /// Edge traversed in reverse direction, implying an indirect/artifact path.
    ReversedEdge,
    /// Path passes through test/spec file nodes (production-code user should
    /// be aware that the reported path is through test infrastructure).
    TestContamination,
    /// Path traverses a function pointer or virtual dispatch edge (Instantiates
    /// or Implements edges, which represent indirect dispatch).
    IndirectCall,
}

impl PathBreakpointKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PathBreakpointKind::LowConfidence => "low_confidence",
            PathBreakpointKind::CallbackRegistration => "callback_registration",
            PathBreakpointKind::ReversedEdge => "reversed_edge",
            PathBreakpointKind::TestContamination => "test_contamination",
            PathBreakpointKind::IndirectCall => "indirect_call",
        }
    }
}

/// Description of a gap or indirect hop in a path.
#[derive(Debug, Clone)]
pub struct PathBreakpoint {
    pub kind: PathBreakpointKind,
    /// Index into the path's edges vector.
    pub edge_index: usize,
    /// Qualified name of the source node for this edge.
    pub from_node: String,
    /// Qualified name of the target node for this edge.
    pub to_node: String,
    /// Human-readable explanation of the breakpoint.
    pub message: String,
}

/// Enhanced path between two nodes with direction, confidence, and breakpoints.
#[derive(Debug, Clone)]
pub struct GraphPath {
    /// Node indices in traversal order (from → ... → to).
    pub node_indices: Vec<NodeIx>,
    /// Per-edge metadata with traversal direction.
    pub edges: Vec<PathEdge>,
    /// Average confidence of edges in the path (0.0 – 1.0).
    /// 1.0 for zero-length paths.
    pub confidence: f64,
    /// Breakpoints describing indirections, low-confidence hops, or other
    /// structural notes. Empty for a clean direct call chain.
    pub breakpoints: Vec<PathBreakpoint>,
    /// Convenience: raw edge indices (computed from edges field).
    /// Kept for backward compatibility with existing callers.
    pub edge_indices: Vec<EdgeIx>,
}

impl GraphPath {
    /// Produce a GraphPath from the inner tuple returned by older APIs.
    /// Note: direction and breakpoints cannot be reconstructed, so they
    /// are omitted. Confidence is set to 1.0.
    #[allow(dead_code)]
    fn from_raw(node_indices: Vec<NodeIx>, edge_indices: Vec<EdgeIx>) -> Self {
        let edges: Vec<PathEdge> = edge_indices
            .iter()
            .map(|&eix| PathEdge {
                edge_ix: eix,
                direction: PathEdgeDirection::Forward,
            })
            .collect();
        let confidence = if edges.is_empty() { 1.0 } else { 1.0 };
        Self {
            node_indices,
            edges,
            confidence,
            breakpoints: vec![],
            edge_indices,
        }
    }
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
        let id = types::ids::SymbolId::generate(&file_id, "typescript", qname, kind.as_str(), None);
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
        let path = snap.shortest_path(a_ix, c_ix, 5, None, TraversalDirection::Both, false).unwrap();
        assert_eq!(path.node_indices.len(), 3);
        assert_eq!(path.edge_indices.len(), 2); // a→b, b→c
        assert!((path.confidence - 1.0).abs() < 0.001);
        assert!(path.breakpoints.is_empty());
        // Edges should be forward (a calls b, b calls c)
        assert_eq!(path.edges.len(), 2);
        for edge in &path.edges {
            assert_eq!(edge.direction, PathEdgeDirection::Forward);
        }
    }

    #[test]
    fn test_graph_shortest_path_edge_filter() {
        let fid = make_file_id("test.ts");
        let a = make_symbol(fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(fid, "b", "b", SymbolKind::Function);
        let c = make_symbol(fid, "c", "c", SymbolKind::Struct);
        // a calls b (Calls edge), b references c (References edge)
        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);
        let e2 = make_edge(b.id, c.id, EdgeKind::References);

        let snap =
            GraphSnapshot::from_parts(vec![a.clone(), b.clone(), c.clone()], vec![e1, e2], 0.0)
                .unwrap();

        let a_ix = snap.id_to_idx[&a.id];
        let c_ix = snap.id_to_idx[&c.id];

        // With calls-only filter: no path (References edge blocked)
        let result = snap.shortest_path(a_ix, c_ix, 5, Some(&[EdgeKind::Calls]), TraversalDirection::Both, false);
        assert!(result.is_none(), "calls-only filter should block References edge");

        // With no filter: path exists through references edge
        let path = snap.shortest_path(a_ix, c_ix, 5, None, TraversalDirection::Both, false).unwrap();
        assert_eq!(path.node_indices.len(), 3);
        assert_eq!(path.edge_indices.len(), 2);
    }

    #[test]
    fn test_shortest_path_reverse_direction() {
        // Test that reverse traversal is detected and annotated
        let fid = make_file_id("test.ts");
        let a = make_symbol(fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(fid, "b", "b", SymbolKind::Function);
        // a calls b — edge direction is a → b
        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);

        let snap =
            GraphSnapshot::from_parts(vec![a.clone(), b.clone()], vec![e1], 0.0)
                .unwrap();

        let b_ix = snap.id_to_idx[&b.id];
        let a_ix = snap.id_to_idx[&a.id];

        // Path from b → a must traverse edge in reverse
        let path = snap.shortest_path(b_ix, a_ix, 5, None, TraversalDirection::Both, false).unwrap();
        assert_eq!(path.node_indices.len(), 2);
        assert_eq!(path.edge_indices.len(), 1);
        assert_eq!(path.edges[0].direction, PathEdgeDirection::Reverse);
        // Should have a ReversedEdge breakpoint
        assert!(path.breakpoints.iter().any(|bp| bp.kind == PathBreakpointKind::ReversedEdge));
    }

    #[test]
    fn test_shortest_path_prefer_production() {
        // Set up: production chain a→b→c and a test file "test.ts" with direct edge test_fn→c
        let prod_fid = make_file_id("lib.rs");
        let test_fid = make_file_id("tests/test.rs");

        let a = make_symbol(prod_fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(prod_fid, "b", "b", SymbolKind::Function);
        let c = make_symbol(prod_fid, "c", "c", SymbolKind::Function);
        let test_fn = make_symbol(test_fid, "test_fn", "test_fn", SymbolKind::Function);

        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);
        let e2 = make_edge(b.id, c.id, EdgeKind::Calls);
        let e3 = make_edge(test_fn.id, c.id, EdgeKind::Calls);

        let mut file_paths = HashMap::new();
        file_paths.insert(prod_fid, "lib.rs".to_string());
        file_paths.insert(test_fid, "tests/test.rs".to_string());

        let snap = GraphSnapshot::from_parts_with_paths(
            vec![a.clone(), b.clone(), c.clone(), test_fn.clone()],
            vec![e1, e2, e3],
            0.0,
            &file_paths,
        )
        .unwrap();

        let a_ix = snap.id_to_idx[&a.id];
        let c_ix = snap.id_to_idx[&c.id];

        // Without prefer_production: BFS may find a→b→c (2 hops) or a↔test_fn→c (bidirectional path through test),
        // but the standard BFS visits from a's outgoing: a→b (production), then b's outgoing: b→c.
        // The path should be the direct production path a→b→c.
        let path = snap.shortest_path(a_ix, c_ix, 5, None, TraversalDirection::Both, false).unwrap();
        assert_eq!(path.node_indices.len(), 3);

        // With prefer_production: same result expected (production path is also shortest)
        let path = snap.shortest_path(a_ix, c_ix, 5, None, TraversalDirection::Both, true).unwrap();
        assert_eq!(path.node_indices.len(), 3);
    }

    #[test]
    fn test_shortest_path_direction_outgoing_only() {
        // Test that direction=Outgoing only follows forward edges
        let fid = make_file_id("test.ts");
        let a = make_symbol(fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(fid, "b", "b", SymbolKind::Function);
        let c = make_symbol(fid, "c", "c", SymbolKind::Function);
        // a calls b (forward edge a→b), b calls c (forward edge b→c)
        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);
        let e2 = make_edge(b.id, c.id, EdgeKind::Calls);

        let snap =
            GraphSnapshot::from_parts(vec![a.clone(), b.clone(), c.clone()], vec![e1, e2], 0.0)
                .unwrap();

        let a_ix = snap.id_to_idx[&a.id];
        let c_ix = snap.id_to_idx[&c.id];

        // Outgoing direction: a→b→c should be found
        let path = snap.shortest_path(a_ix, c_ix, 5, None, TraversalDirection::Outgoing, false).unwrap();
        assert_eq!(path.node_indices.len(), 3);

        // Reverse: from c back to a requires incoming edges, so Outgoing should find no path
        let result = snap.shortest_path(c_ix, a_ix, 5, None, TraversalDirection::Outgoing, false);
        assert!(result.is_none(), "Outgoing-only from c→a should fail (edges go a→b→c)");
    }

    #[test]
    fn test_shortest_path_direction_incoming_only() {
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

        // Incoming direction: from c back to a (following callers/called-by edges) should work
        let path = snap.shortest_path(c_ix, a_ix, 5, None, TraversalDirection::Incoming, false).unwrap();
        assert_eq!(path.node_indices.len(), 3);

        // Forward (a→c) via incoming-only should fail
        let result = snap.shortest_path(a_ix, c_ix, 5, None, TraversalDirection::Incoming, false);
        assert!(result.is_none(), "Incoming-only from a→c should fail");
    }

    #[test]
    fn test_is_likely_test_path_patterns() {
        // Directory-based patterns
        assert!(super::is_likely_test_path("tests/test.c"));
        assert!(super::is_likely_test_path("src/__tests__/foo.py"));
        assert!(super::is_likely_test_path("spec/models/user_spec.rb"));
        assert!(super::is_likely_test_path("test/unit_test.go"));
        assert!(super::is_likely_test_path("testing/integration_test.py"));
        // Name-based patterns
        assert!(super::is_likely_test_path("src/foo_test.rs"));
        assert!(super::is_likely_test_path("lib/user.test.ts"));
        assert!(super::is_likely_test_path("components/Button.spec.tsx"));
        assert!(super::is_likely_test_path("src/test_utils.py"));
        assert!(super::is_likely_test_path("test_file.c"));
        assert!(super::is_likely_test_path("test.c"));
        // Non-test paths should return false
        assert!(!super::is_likely_test_path("src/main.rs"));
        assert!(!super::is_likely_test_path("lib/http.c"));
        assert!(!super::is_likely_test_path("components/Button.tsx"));
        assert!(!super::is_likely_test_path("util/contest.c")); // "test" not in path
    }

    #[test]
    fn test_breakpoints_low_confidence() {
        let fid = make_file_id("lib.rs");
        let a = make_symbol(fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(fid, "b", "b", SymbolKind::Function);
        let mut e = make_edge(a.id, b.id, EdgeKind::Calls);
        e.confidence = Confidence::new(0.3); // low confidence

        let file_paths = std::collections::HashMap::from([(fid, "lib.rs".to_string())]);

        let snap = GraphSnapshot::from_parts_with_paths(
            vec![a.clone(), b.clone()],
            vec![e],
            0.0, // keep all edges
            &file_paths,
        )
        .unwrap();

        let a_ix = snap.id_to_idx[&a.id];
        let b_ix = snap.id_to_idx[&b.id];
        let path = snap.shortest_path(a_ix, b_ix, 5, None, TraversalDirection::Both, false).unwrap();
        assert_eq!(path.node_indices.len(), 2);
        assert!(path.confidence < 0.5, "confidence should be < 0.5");
        assert!(path.breakpoints.iter().any(|bp| bp.kind == PathBreakpointKind::LowConfidence),
            "should have LowConfidence breakpoint");
    }

    #[test]
    fn test_breakpoints_indirect_call() {
        let fid = make_file_id("lib.rs");
        let a = make_symbol(fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(fid, "Handler", "Handler", SymbolKind::Interface);
        let e = make_edge(a.id, b.id, EdgeKind::Implements);

        let file_paths = std::collections::HashMap::from([(fid, "lib.rs".to_string())]);

        let snap = GraphSnapshot::from_parts_with_paths(
            vec![a.clone(), b.clone()],
            vec![e],
            0.0,
            &file_paths,
        )
        .unwrap();

        let a_ix = snap.id_to_idx[&a.id];
        let b_ix = snap.id_to_idx[&b.id];
        let path = snap.shortest_path(a_ix, b_ix, 5, None, TraversalDirection::Both, false).unwrap();
        assert!(path.breakpoints.iter().any(|bp| bp.kind == PathBreakpointKind::IndirectCall),
            "should have IndirectCall breakpoint for Implements edge");
    }

    #[test]
    fn test_breakpoints_test_contamination() {
        let prod_fid = make_file_id("lib.rs");
        let test_fid = make_file_id("tests/test.rs");

        let prod_fn = make_symbol(prod_fid, "do_work", "do_work", SymbolKind::Function);
        let test_fn = make_symbol(test_fid, "test_do_work", "test_do_work", SymbolKind::Function);
        // test_fn calls prod_fn (forward edge: test_fn → prod_fn)
        let e = make_edge(test_fn.id, prod_fn.id, EdgeKind::Calls);

        let mut file_paths = std::collections::HashMap::new();
        file_paths.insert(prod_fid, "lib.rs".to_string());
        file_paths.insert(test_fid, "tests/test.rs".to_string());

        let snap = GraphSnapshot::from_parts_with_paths(
            vec![prod_fn.clone(), test_fn.clone()],
            vec![e],
            0.0,
            &file_paths,
        )
        .unwrap();

        let prod_ix = snap.id_to_idx[&prod_fn.id];
        let test_ix = snap.id_to_idx[&test_fn.id];

        // Path from prod_fn → test_fn traverses edge in reverse: prod_fn is target of test_fn's call
        // So the edge is traversed in reverse from prod_fn (target) to test_fn (source)
        let path = snap.shortest_path(prod_ix, test_ix, 5, None, TraversalDirection::Both, false).unwrap();
        // Should have TestContamination breakpoint (test_fn is in tests/ directory)
        assert!(path.breakpoints.iter().any(|bp| bp.kind == PathBreakpointKind::TestContamination),
            "should have TestContamination breakpoint for test file node");
    }

    #[test]
    fn test_shortest_path_prefer_production_avoids_test() {
        // Production chain: a → b → c  (3 hops)
        // Test shortcut:     a → test_fn → c (2 hops via test file)
        // prefer_production should prefer the 3-hop production path
        let prod_fid = make_file_id("lib.rs");
        let test_fid = make_file_id("tests/test.rs");

        let a = make_symbol(prod_fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(prod_fid, "b", "b", SymbolKind::Function);
        let c = make_symbol(prod_fid, "c", "c", SymbolKind::Function);
        let test_fn = make_symbol(test_fid, "test_fn", "test_fn", SymbolKind::Function);

        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);
        let e2 = make_edge(b.id, c.id, EdgeKind::Calls);
        let e3 = make_edge(a.id, test_fn.id, EdgeKind::Calls);
        let e4 = make_edge(test_fn.id, c.id, EdgeKind::Calls);

        let mut file_paths = std::collections::HashMap::new();
        file_paths.insert(prod_fid, "lib.rs".to_string());
        file_paths.insert(test_fid, "tests/test.rs".to_string());

        let snap = GraphSnapshot::from_parts_with_paths(
            vec![a.clone(), b.clone(), c.clone(), test_fn.clone()],
            vec![e1, e2, e3, e4],
            0.0,
            &file_paths,
        )
        .unwrap();

        let a_ix = snap.id_to_idx[&a.id];
        let c_ix = snap.id_to_idx[&c.id];

        // Without prefer_production: BFS may find a→b→c (3 hops) or a→test_fn→c (2 hops)
        // Standard BFS finds shortest = 2 hops through test
        let path_no_pref = snap.shortest_path(a_ix, c_ix, 5, None, TraversalDirection::Both, false).unwrap();
        assert_eq!(path_no_pref.node_indices.len(), 3); // 3 nodes = 2 hops (a→test_fn→c)

        // With prefer_production: should prefer 3-hop production path (a→b→c)
        let path_pref = snap.shortest_path(a_ix, c_ix, 5, None, TraversalDirection::Both, true).unwrap();
        assert_eq!(path_pref.node_indices.len(), 3); // 3 nodes = 2 hops
        // The path should go through b (production), not test_fn
        let middle_node = &snap.nodes[path_pref.node_indices[1]];
        assert_eq!(middle_node.name, "b", "prefer_production should route through b, not test_fn");
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
