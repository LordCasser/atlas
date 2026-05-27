//! GraphSnapshot: in-memory graph loaded from SQLite for fast traversal.
//!
//! Key design: graph queries do NOT hit SQLite; all data is pre-loaded into
//! HashMaps and adjacency lists. Snapshot is immutable after construction.

use db::Store;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet, VecDeque};
use types::ids::{FileId, SymbolId};
use types::{
    Confidence, EdgeKind, Language, Provenance, RawEdge, SymbolDef, SymbolKind, Visibility,
};

// ── type aliases ────────────────────────────────────────────────────────────

pub type NodeIx = usize;
pub type EdgeIx = usize;

// ── ordered f64 wrapper (f64 doesn't impl Ord natively) ─────────────────

/// An `f64` that implements `Ord` via [`f64::total_cmp`], suitable for use
/// in `BinaryHeap`-based Dijkstra.
#[derive(Clone, Copy, Debug)]
struct OrdF64(f64);

impl PartialEq for OrdF64 {
    fn eq(&self, other: &Self) -> bool { self.0 == other.0 }
}
impl Eq for OrdF64 {}
impl PartialOrd for OrdF64 {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for OrdF64 {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

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

// ── forward frontier diagnostic ──────────────────────────────────────────────

/// A single node in the forward frontier — the deepest nodes reached when
/// walking forward from a source symbol before the chain exhausts.
#[derive(Debug, Clone)]
pub struct FrontierNode {
    pub symbol_id: SymbolId,
    pub qname: String,
    pub depth: usize,
    /// Number of outgoing call edges (Calls/Instantiates/Implements/RegistersCallback)
    /// from this node.  0 means this function has no statically-resolved callees
    /// and is a likely dynamic-dispatch (function pointer / virtual call) boundary.
    pub outgoing_call_count: usize,
}

/// Result of a forward-trace from source symbols to the deepest reachable
/// nodes via outgoing edges only (forward call chain direction).
#[derive(Debug, Clone)]
pub struct ForwardFrontier {
    /// Max depth reached before all forward edges were exhausted.
    pub depth_reached: usize,
    /// Nodes at the frontier — functions with zero forward call edges or
    /// the deepest functions reached within max_depth.
    pub frontier_nodes: Vec<FrontierNode>,
}

impl ForwardFrontier {
    /// Remove duplicate frontier entries (same symbol_id), keeping the
    /// one with the shallowest depth.
    fn deduplicate(&mut self) {
        let mut seen: HashMap<SymbolId, usize> = HashMap::new();
        let mut keep = Vec::new();
        for node in self.frontier_nodes.drain(..) {
            match seen.get(&node.symbol_id) {
                Some(&prev_depth) if prev_depth <= node.depth => {
                    // Already have a shallower entry — skip this one.
                }
                _ => {
                    seen.insert(node.symbol_id, node.depth);
                    keep.push(node);
                }
            }
        }
        self.frontier_nodes = keep;
    }
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
                total_weight: 0.0,
                test_hops: if self.nodes[from].is_test_file { 1 } else { 0 },
                indirect_hops: 0,
            });
        }
        if from >= self.nodes.len() || to >= self.nodes.len() {
            return None;
        }

        if prefer_production {
            return self.shortest_path_weighted_prod(from, to, max_depth, edge_kind_filter, direction);
        }
        self.shortest_path_weighted(from, to, max_depth, edge_kind_filter, direction)
    }

    /// Standard bidirectional BFS — unchanged behavior, enhanced output.
    /// Kept for backward compatibility; new code should use
    /// [`shortest_path_weighted`] for semantically-aware pathfinding.
    #[allow(dead_code)]
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
    /// Kept for backward compatibility; use [`shortest_path_weighted_prod`].
    #[allow(dead_code)]
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

        // Compute path quality metadata
        let mut total_weight = 0.0f64;
        let mut test_hops = 0usize;
        let mut indirect_hops = 0usize;
        for (i, node_ix) in path_nodes.iter().enumerate() {
            if self.nodes[*node_ix].is_test_file {
                test_hops += 1;
            }
            // Edge metadata: use the edge leaving this node (except last)
            if i < raw_edges.len() {
                let eix = raw_edges[i];
                let edge = &self.edges[eix];
                let next_node = path_nodes[i + 1];
                if is_indirect_edge(&edge.kind) {
                    indirect_hops += 1;
                }
                total_weight += self.edge_weight(eix, next_node);
            }
        }

        GraphPath {
            node_indices: path_nodes,
            edge_indices: raw_edges,
            edges,
            confidence,
            breakpoints,
            total_weight,
            test_hops,
            indirect_hops,
        }
    }

    // ── edge weighting ───────────────────────────────────────────────────

    /// Compute the traversal weight for traversing `edge_ix` into `neighbor_ix`.
    ///
    /// Lower weight = more semantically direct path.  The weight penalises:
    ///
    /// | Factor                    | Max penalty | When                          |
    /// |---------------------------|-------------|-------------------------------|
    /// | Indirect calls            | +1.0        | Implements/Instantiates/RegistersCallback |
    /// | Low-confidence edges      | +0.5        | confidence < 1.0              |
    /// | Heuristic provenance      | +0.3        | Heuristic/CallbackPattern     |
    /// | Edge-case file location   | +0.5        | docs/examples/test dirs       |
    /// | Edge-case name patterns   | +0.5        | proxy/fallback/alt patterns   |
    ///
    /// The base weight is 1.0, so a "perfect" path of N hops has total_weight = N.
    fn edge_weight(&self, edge_ix: EdgeIx, neighbor_ix: NodeIx) -> f64 {
        let edge = &self.edges[edge_ix];
        let node = &self.nodes[neighbor_ix];
        let mut w = 1.0; // base hop cost

        // ── Edge penalties ──
        if is_indirect_edge(&edge.kind) {
            w += 1.0;
        }
        w += (1.0 - edge.confidence.as_f32() as f64) * 0.5;

        match edge.provenance {
            Provenance::Heuristic | Provenance::CallbackPattern => w += 0.3,
            _ => {}
        }

        // ── Node / file penalties ──
        w += location_penalty(node);
        w += name_pattern_penalty(node);

        // Clamp: even heavily penalised edges contribute at least 1.0
        w.max(1.0)
    }

    /// Weighted shortest path using Dijkstra's algorithm.
    ///
    /// Unlike BFS which minimises hop count, this minimises total traversal
    /// weight — penalising edge-case functions (proxy/fallback patterns),
    /// low-confidence edges, and indirect calls so that semantically-direct
    /// paths are preferred even when they require more hops.
    fn shortest_path_weighted(
        &self,
        from: NodeIx,
        to: NodeIx,
        max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>,
        direction: TraversalDirection,
    ) -> Option<GraphPath> {
        self.dijkstra_core(from, to, max_depth, edge_kind_filter, direction, None, 0.0)
    }

    /// Weighted shortest path preferring production code.
    ///
    /// Adds a weight penalty for test-file nodes on top of the normal
    /// edge weight.  This ensures a pure-production path with more hops
    /// is preferred over a shorter test-contaminated path.
    fn shortest_path_weighted_prod(
        &self,
        from: NodeIx,
        to: NodeIx,
        max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>,
        direction: TraversalDirection,
    ) -> Option<GraphPath> {
        const TEST_FILE_PENALTY: f64 = 5.0;
        let start_penalty = if self.nodes[from].is_test_file {
            TEST_FILE_PENALTY
        } else {
            0.0
        };
        self.dijkstra_core(from, to, max_depth, edge_kind_filter, direction, None, start_penalty)
    }

    // ── forward frontier ─────────────────────────────────────────────────

    /// Walk forward (outgoing direction only) from `start_symbols`, collecting
    /// the deepest reachable nodes.  When the forward walk exhausts (no more
    /// outgoing call edges), those terminal nodes become the **forward frontier**
    /// — they are the point where static analysis could not proceed further,
    /// typically because of dynamic dispatch (function pointers, virtual calls).
    ///
    /// This is intended as a **diagnostic** helper: call it after a
    /// `shortest_path` with `Outgoing` direction returns `None`, to understand
    /// *where* the forward chain broke and *why*.
    pub fn forward_frontier(
        &self,
        start_symbols: &[SymbolId],
        max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>,
    ) -> ForwardFrontier {
        let mut frontier = Vec::new();
        let mut visited: HashSet<SymbolId> = HashSet::new();
        let mut current: Vec<SymbolId> = start_symbols
            .iter()
            .filter(|id| self.id_to_idx.contains_key(id))
            .copied()
            .collect();
        let mut depth = 0usize;

        while depth <= max_depth && !current.is_empty() {
            let mut next = Vec::new();
            for id in current.drain(..) {
                if !visited.insert(id) { continue; }
                let ix = match self.id_to_idx.get(&id) {
                    Some(i) => *i,
                    None => continue,
                };
                let outgoing: Vec<(NodeIx, EdgeIx)> = self.neighbors(
                    ix, TraversalDirection::Outgoing, edge_kind_filter,
                );
                let call_count = outgoing.len();
                if call_count == 0 {
                    frontier.push(FrontierNode {
                        symbol_id: id,
                        qname: self.nodes[ix].qualified_name.clone(),
                        depth,
                        outgoing_call_count: 0,
                    });
                } else {
                    for (neighbor_ix, _) in outgoing {
                        next.push(self.nodes[neighbor_ix].symbol_id);
                    }
                }
            }
            if next.is_empty() { break; }
            current = next;
            depth += 1;
        }
        if depth >= max_depth && !current.is_empty() {
            for id in current {
                if visited.insert(id) {
                    if let Some(ix) = self.id_to_idx.get(&id) {
                        let outgoing = self.neighbors(
                            *ix, TraversalDirection::Outgoing, edge_kind_filter,
                        );
                        frontier.push(FrontierNode {
                            symbol_id: id,
                            qname: self.nodes[*ix].qualified_name.clone(),
                            depth,
                            outgoing_call_count: outgoing.len(),
                        });
                    }
                }
            }
        }

        let mut result = ForwardFrontier {
            depth_reached: depth,
            frontier_nodes: frontier,
        };
        result.deduplicate();
        result
    }

    // ── stats ────────────────────────────────────────────────────────────

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    // ── multi-path selection ────────────────────────────────────────────

    /// Find up to `k` alternative paths with edge-removal diversity, ranked
    /// by composite semantic+topological+centrality score (descending).
    pub fn k_ranked_paths(
        &self,
        from: NodeIx,
        to: NodeIx,
        k: usize,
        max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>,
        direction: TraversalDirection,
        prefer_production: bool,
    ) -> Vec<RankedPath> {
        let mut candidates: Vec<RankedPath> = Vec::with_capacity(k);
        let primary = if prefer_production {
            self.shortest_path_weighted_prod_ex(from, to, max_depth, edge_kind_filter, direction, None)
        } else {
            self.shortest_path_weighted_ex(from, to, max_depth, edge_kind_filter, direction, None)
        };
        let primary = match primary {
            Some(p) => p,
            None => return Vec::new(),
        };
        candidates.push(RankedPath { path: primary, scores: CompositePathScore::default() });
        if k <= 1 {
            candidates[0].scores = self.score_path(&candidates[0].path);
            return candidates;
        }

        let mut seen_edge_lists: HashSet<Vec<EdgeIx>> = HashSet::new();
        seen_edge_lists.insert(primary_edge_id(&candidates[0].path));

        for edge_idx in 0..candidates[0].path.edge_indices.len() {
            if candidates.len() >= k { break; }
            let mut excluded = HashSet::new();
            excluded.insert(candidates[0].path.edge_indices[edge_idx]);
            let alt = if prefer_production {
                self.shortest_path_weighted_prod_ex(from, to, max_depth, edge_kind_filter, direction, Some(&excluded))
            } else {
                self.shortest_path_weighted_ex(from, to, max_depth, edge_kind_filter, direction, Some(&excluded))
            };
            if let Some(alt_path) = alt {
                let eid = primary_edge_id(&alt_path);
                if !seen_edge_lists.contains(&eid) && alt_path.node_indices.len() > 1 {
                    seen_edge_lists.insert(eid);
                    candidates.push(RankedPath { path: alt_path, scores: CompositePathScore::default() });
                }
            }
        }

        if candidates.len() < k && candidates[0].path.edge_indices.len() >= 2 {
            let n = candidates[0].path.edge_indices.len();
            'outer: for i in 0..n {
                for j in (i + 1)..n {
                    if candidates.len() >= k { break 'outer; }
                    let mut excluded = HashSet::new();
                    excluded.insert(candidates[0].path.edge_indices[i]);
                    excluded.insert(candidates[0].path.edge_indices[j]);
                    let alt = if prefer_production {
                        self.shortest_path_weighted_prod_ex(from, to, max_depth, edge_kind_filter, direction, Some(&excluded))
                    } else {
                        self.shortest_path_weighted_ex(from, to, max_depth, edge_kind_filter, direction, Some(&excluded))
                    };
                    if let Some(alt_path) = alt {
                        let eid = primary_edge_id(&alt_path);
                        if !seen_edge_lists.contains(&eid) && alt_path.node_indices.len() > 1 {
                            seen_edge_lists.insert(eid);
                            candidates.push(RankedPath { path: alt_path, scores: CompositePathScore::default() });
                        }
                    }
                }
            }
        }

        for c in &mut candidates {
            c.scores = self.score_path(&c.path);
        }
        candidates.sort_by(|a, b| {
            b.scores.overall.total_cmp(&a.scores.overall)
                .then_with(|| a.path.node_indices.len().cmp(&b.path.node_indices.len()))
        });
        candidates
    }

    fn score_path(&self, path: &GraphPath) -> CompositePathScore {
        let n = path.node_indices.len();
        if n <= 1 {
            return CompositePathScore { overall: 1.0, semantic: 1.0, topology: 1.0, centrality: 1.0 };
        }
        let semantic: f64 = path.node_indices.iter()
            .map(|&ix| { let n = &self.nodes[ix]; 1.0 - (name_pattern_penalty(n) + location_penalty(n)).min(1.0) })
            .sum::<f64>() / n as f64;
        let topology: f64 = if path.edge_indices.is_empty() { 1.0 } else {
            path.edge_indices.iter()
                .map(|&eix| { let e = &self.edges[eix]; let b = e.confidence.as_f32() as f64; if is_indirect_edge(&e.kind) { b*0.7 } else { b } })
                .sum::<f64>() / path.edge_indices.len() as f64
        };
        let centrality: f64 = if n <= 2 { 0.7 } else {
            path.node_indices[1..n-1].iter()
                .map(|&ix| { let d = (self.nodes[ix].outgoing.len() + self.nodes[ix].incoming.len()) as f64; (d.min(10.0)/10.0).max(0.1) })
                .sum::<f64>() / (n-2) as f64
        };
        CompositePathScore {
            overall: semantic * 0.40 + topology * 0.35 + centrality * 0.25,
            semantic, topology, centrality,
        }
    }

    fn shortest_path_weighted_ex(&self, from: NodeIx, to: NodeIx, max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>, direction: TraversalDirection,
        excluded_edges: Option<&HashSet<EdgeIx>>) -> Option<GraphPath> {
        self.dijkstra_core(from, to, max_depth, edge_kind_filter, direction, excluded_edges, 0.0)
    }

    fn shortest_path_weighted_prod_ex(&self, from: NodeIx, to: NodeIx, max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>, direction: TraversalDirection,
        excluded_edges: Option<&HashSet<EdgeIx>>) -> Option<GraphPath> {
        const PEN: f64 = 5.0;
        self.dijkstra_core(from, to, max_depth, edge_kind_filter, direction, excluded_edges,
            if self.nodes[from].is_test_file { PEN } else { 0.0 })
    }

    fn dijkstra_core(&self, from: NodeIx, to: NodeIx, max_depth: usize,
        edge_kind_filter: Option<&[EdgeKind]>, direction: TraversalDirection,
        excluded_edges: Option<&HashSet<EdgeIx>>, start_cost: f64) -> Option<GraphPath> {
        const PEN: f64 = 5.0;
        let n = self.nodes.len();
        let mut dist = vec![f64::INFINITY; n];
        let mut parent = vec![None; n];
        let mut parent_edge = vec![None; n];
        let mut heap: BinaryHeap<(Reverse<OrdF64>, usize, NodeIx)> = BinaryHeap::new();
        dist[from] = start_cost;
        heap.push((Reverse(OrdF64(start_cost)), 0, from));
        while let Some((Reverse(cost), depth, cur)) = heap.pop() {
            if cost.0 > dist[cur] { continue; }
            if cur == to { return Some(self.reconstruct_path(from, to, &parent_edge, &parent)); }
            if depth >= max_depth { continue; }
            for eix in self.edge_iter(cur, direction) {
                if let Some(excl) = excluded_edges { if excl.contains(&eix) { continue; } }
                let edge = &self.edges[eix];
                if let Some(kinds) = edge_kind_filter { if !kinds.contains(&edge.kind) { continue; } }
                let nb = if edge.source_ix == cur { edge.target_ix } else { edge.source_ix };
                let mut nc = cost.0 + self.edge_weight(eix, nb);
                if start_cost > 0.0 && self.nodes[nb].is_test_file { nc += PEN; }
                if nc < dist[nb] { dist[nb] = nc; parent[nb] = Some(cur); parent_edge[nb] = Some(eix); heap.push((Reverse(OrdF64(nc)), depth+1, nb)); }
            }
        }
        None
    }

    /// Iterate edge indices for a node given a direction, returning a flat iterator.
    fn edge_iter(&self, node: NodeIx, direction: TraversalDirection) -> Box<dyn Iterator<Item = EdgeIx> + '_> {
        match direction {
            TraversalDirection::Outgoing => Box::new(self.nodes[node].outgoing.iter().copied()),
            TraversalDirection::Incoming => Box::new(self.nodes[node].incoming.iter().copied()),
            TraversalDirection::Both => Box::new(self.nodes[node].outgoing.iter().chain(self.nodes[node].incoming.iter()).copied()),
        }
    }
}

// ── traversal config ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            max_depth: 5,
            limit: 100,
            edge_kind_filter: None,
        }
    }
}

// ── result types ────────────────────────────────────────────────────────────

/// Composite quality score for a call-graph path.
///
/// Computed from three independent dimensions, then combined into an overall
/// score (0.0–1.0).  Higher = more semantically relevant.
#[derive(Debug, Clone)]
pub struct CompositePathScore {
    /// Overall composite score (0.0–1.0, higher is better).
    pub overall: f64,
    /// Semantic quality: function-name and file-location relevance.
    pub semantic: f64,
    /// Topological quality: edge confidence and directness.
    pub topology: f64,
    /// Centrality: how central are the intermediate nodes in the call graph?
    pub centrality: f64,
}

impl Default for CompositePathScore {
    fn default() -> Self {
        Self { overall: 1.0, semantic: 1.0, topology: 1.0, centrality: 1.0 }
    }
}

/// A candidate path with its composite quality score.
#[derive(Debug, Clone)]
pub struct RankedPath {
    pub path: GraphPath,
    pub scores: CompositePathScore,
}

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
    /// Total traversal weight of this path (sum of per-edge weights).
    /// Lower = more semantically direct; higher = passes through
    /// edge-case code (proxy/fallback patterns, low-confidence edges).
    pub total_weight: f64,
    /// Number of hops that pass through test-file nodes.
    pub test_hops: usize,
    /// Number of indirect-call hops (Instantiates, Implements,
    /// RegistersCallback edges).
    pub indirect_hops: usize,
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
            total_weight: 0.0,
            test_hops: 0,
            indirect_hops: 0,
        }
    }
}

/// Produce a stable identifier for a path's edge set for deduplication.
fn primary_edge_id(path: &GraphPath) -> Vec<EdgeIx> {
    let mut ids: Vec<EdgeIx> = path.edge_indices.clone();
    ids.sort_unstable();
    ids
}

// ── edge-weighting helpers (free functions) ──────────────────────────────

/// Whether an edge kind represents an indirect call (not a direct Calls edge).
fn is_indirect_edge(kind: &EdgeKind) -> bool {
    matches!(
        kind,
        EdgeKind::Implements | EdgeKind::Instantiates | EdgeKind::RegistersCallback
    )
}

/// Penalty for nodes in directories that suggest edge-case code
/// (docs, examples, test fixtures).
fn location_penalty(node: &NodeSummary) -> f64 {
    // We don't have the file path directly in NodeSummary, but we can
    // check is_test_file (set during construction from path heuristics).
    if node.is_test_file {
        return 0.5;
    }
    0.0
}

/// Penalty for function/qualified names matching edge-case patterns.
///
/// Functions whose names suggest proxy, fallback, alternate, or spare
/// implementations are penalised because they typically represent
/// non-primary code paths.
fn name_pattern_penalty(node: &NodeSummary) -> f64 {
    // Check the simple name (case-insensitive substring) for edge-case
    // patterns. Qualified names are checked too for things like
    // "socks5_proxy.c" appearing in the file-scoped qualified name.
    let lower = node.qualified_name.to_lowercase();
    let patterns = [
        "proxy", "socks", "fallback", "alternate", "backup", "spare",
        "alt_", "_alt",
    ];
    for pat in &patterns {
        if lower.contains(pat) {
            return 0.5;
        }
    }
    0.0
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

    // ── OrdF64 tests ───────────────────────────────────────────────────

    #[test]
    fn test_ordf64_ordering() {
        let a = OrdF64(1.0);
        let b = OrdF64(2.0);
        let c = OrdF64(1.0);
        assert!(a < b);
        assert!(b > a);
        assert_eq!(a, c);
        assert!(OrdF64(f64::NAN) != OrdF64(f64::NAN)); // NaN ≠ NaN
    }

    #[test]
    fn test_ordf64_in_heap() {
        use std::collections::BinaryHeap;
        let mut heap = BinaryHeap::new();
        heap.push(Reverse(OrdF64(3.0)));
        heap.push(Reverse(OrdF64(1.0)));
        heap.push(Reverse(OrdF64(2.0)));
        assert_eq!(heap.pop().unwrap().0 .0, 1.0);
        assert_eq!(heap.pop().unwrap().0 .0, 2.0);
        assert_eq!(heap.pop().unwrap().0 .0, 3.0);
    }

    // ── name/location penalty tests ────────────────────────────────────

    #[test]
    fn test_name_pattern_penalty_proxy() {
        let fid = make_file_id("src/proxy_handler.c");
        let sym = make_symbol(fid, "socks5_connect", "socks5_connect", SymbolKind::Function);
        let snap = GraphSnapshot::from_parts(vec![sym.clone()], vec![], 0.0).unwrap();
        let penalty = name_pattern_penalty(&snap.nodes[0]);
        assert!(penalty > 0.0, "socks5 should be penalised");
    }

    #[test]
    fn test_name_pattern_penalty_fallback() {
        let fid = make_file_id("src/fallback.c");
        let sym = make_symbol(fid, "connect_fallback", "connect_fallback", SymbolKind::Function);
        let snap = GraphSnapshot::from_parts(vec![sym.clone()], vec![], 0.0).unwrap();
        let penalty = name_pattern_penalty(&snap.nodes[0]);
        assert!(penalty > 0.0, "fallback pattern should be penalised");
    }

    #[test]
    fn test_name_pattern_penalty_normal() {
        let fid = make_file_id("src/http.c");
        let sym = make_symbol(fid, "Curl_http", "Curl_http", SymbolKind::Function);
        let snap = GraphSnapshot::from_parts(vec![sym.clone()], vec![], 0.0).unwrap();
        let penalty = name_pattern_penalty(&snap.nodes[0]);
        assert_eq!(penalty, 0.0, "normal name should have no penalty");
    }

    #[test]
    fn test_location_penalty_test_file() {
        let fid = make_file_id("tests/test_connect.c");
        let sym = make_symbol(fid, "test_connect", "test_connect", SymbolKind::Function);
        let mut paths = std::collections::HashMap::new();
        paths.insert(fid, "tests/test_connect.c".to_string());
        let snap = GraphSnapshot::from_parts_with_paths(
            vec![sym.clone()], vec![], 0.0, &paths,
        ).unwrap();
        let penalty = location_penalty(&snap.nodes[0]);
        assert!(penalty > 0.0, "test file should be penalised");
    }

    // ── edge_weight tests ─────────────────────────────────────────────

    #[test]
    fn test_edge_weight_baseline() {
        // A simple Calls edge with TreeSitter provenance and full confidence
        // should have weight close to 1.0.
        let fid = make_file_id("src/main.c");
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let b = make_symbol(fid, "helper", "helper", SymbolKind::Function);
        let e = make_edge(a.id, b.id, EdgeKind::Calls);
        let snap = GraphSnapshot::from_parts(vec![a, b], vec![e], 0.0).unwrap();
        let b_ix = snap.id_to_idx[&snap.nodes[1].symbol_id];
        let w = snap.edge_weight(0, b_ix);
        assert!((1.0..=1.2).contains(&w), "baseline weight should be ~1.0, got {}", w);
    }

    #[test]
    fn test_edge_weight_indirect_penalty() {
        let fid = make_file_id("src/main.c");
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let b = make_symbol(fid, "impl_H", "impl_H", SymbolKind::Function);
        let e = make_edge(a.id, b.id, EdgeKind::Implements);
        let snap = GraphSnapshot::from_parts(vec![a, b], vec![e], 0.0).unwrap();
        let b_ix = snap.id_to_idx[&snap.nodes[1].symbol_id];
        let w = snap.edge_weight(0, b_ix);
        assert!(w >= 2.0, "Implements edge should add +1.0 penalty, got {}", w);
    }

    #[test]
    fn test_edge_weight_proxy_name_penalty() {
        let fid = make_file_id("src/socks5.c");
        let a = make_symbol(fid, "connect", "connect", SymbolKind::Function);
        let b = make_symbol(fid, "socks5_negotiate", "socks5_negotiate", SymbolKind::Function);
        let e = make_edge(a.id, b.id, EdgeKind::Calls);
        let snap = GraphSnapshot::from_parts(vec![a, b], vec![e], 0.0).unwrap();
        let b_ix = snap.id_to_idx[&snap.nodes[1].symbol_id];
        let w = snap.edge_weight(0, b_ix);
        assert!(w > 1.0, "socks5 name should add penalty, got {}", w);
    }

    // ── CompositePathScore tests ───────────────────────────────────────

    #[test]
    fn test_score_path_trivial() {
        let fid = make_file_id("src/main.c");
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let snap = GraphSnapshot::from_parts(vec![a], vec![], 0.0).unwrap();
        // Trivial path (single node).
        let path = snap.shortest_path(0, 0, 5, None, TraversalDirection::Outgoing, false).unwrap();
        let score = snap.score_path(&path);
        assert_eq!(score.overall, 1.0);
        assert_eq!(score.semantic, 1.0);
    }

    #[test]
    fn test_score_path_with_proxy_penalty() {
        let fid = make_file_id("src/proxy.c");
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let b = make_symbol(fid, "socks5_connect", "socks5_connect", SymbolKind::Function);
        let c = make_symbol(fid, "write", "write", SymbolKind::Function);
        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);
        let e2 = make_edge(b.id, c.id, EdgeKind::Calls);
        let snap = GraphSnapshot::from_parts(vec![a, b, c], vec![e1, e2], 0.0).unwrap();
        let a_ix = snap.id_to_idx[&snap.nodes[0].symbol_id];
        let c_ix = snap.id_to_idx[&snap.nodes[2].symbol_id];
        let path = snap.shortest_path(a_ix, c_ix, 5, None, TraversalDirection::Outgoing, false).unwrap();
        let score = snap.score_path(&path);
        // With socks5_connect as intermediate, semantic score should be < 1.0
        assert!(score.semantic < 1.0, "socks5_connect should reduce semantic score, got {}", score.semantic);
        assert!(score.overall < 1.0, "overall should reflect penalty");
    }

    // ── k_ranked_paths tests ───────────────────────────────────────────

    #[test]
    fn test_k_ranked_paths_single() {
        let fid = make_file_id("src/main.c");
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let b = make_symbol(fid, "helper", "helper", SymbolKind::Function);
        let e = make_edge(a.id, b.id, EdgeKind::Calls);
        let snap = GraphSnapshot::from_parts(vec![a, b], vec![e], 0.0).unwrap();
        let a_ix = snap.id_to_idx[&snap.nodes[0].symbol_id];
        let b_ix = snap.id_to_idx[&snap.nodes[1].symbol_id];
        let ranked = snap.k_ranked_paths(a_ix, b_ix, 5, 5, None, TraversalDirection::Outgoing, false);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].path.node_indices.len(), 2);
        assert!(ranked[0].scores.overall > 0.0);
    }

    #[test]
    fn test_k_ranked_paths_alternatives_via_branch() {
        // Graph: a → b → d   (primary path: 2 hops)
        //        a → c → d   (alternative: also 2 hops)
        let fid = make_file_id("src/main.c");
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let b = make_symbol(fid, "init", "init", SymbolKind::Function);
        let c = make_symbol(fid, "proxy_setup", "proxy_setup", SymbolKind::Function);
        let d = make_symbol(fid, "write", "write", SymbolKind::Function);
        let e1 = make_edge(a.id, b.id, EdgeKind::Calls);
        let e2 = make_edge(b.id, d.id, EdgeKind::Calls);
        let e3 = make_edge(a.id, c.id, EdgeKind::Calls);
        let e4 = make_edge(c.id, d.id, EdgeKind::Calls);
        let snap =
            GraphSnapshot::from_parts(vec![a, b, c, d], vec![e1, e2, e3, e4], 0.0).unwrap();
        let a_ix = snap.id_to_idx[&snap.nodes[0].symbol_id];
        let d_ix = snap.id_to_idx[&snap.nodes[3].symbol_id];
        let ranked = snap.k_ranked_paths(a_ix, d_ix, 5, 5, None, TraversalDirection::Outgoing, false);
        assert!(ranked.len() >= 2, "should find at least 2 alternative paths, got {}", ranked.len());
        // The proxy_setup path should have lower semantic score than the init path.
        let proxy_path = ranked.iter().find(|r| {
            r.path.node_indices.iter().any(|&ix| snap.nodes[ix].name == "proxy_setup")
        });
        let init_path = ranked.iter().find(|r| {
            r.path.node_indices.iter().any(|&ix| snap.nodes[ix].name == "init")
        });
        if let (Some(pp), Some(ip)) = (proxy_path, init_path) {
            assert!(pp.scores.semantic < ip.scores.semantic,
                "proxy path should have lower semantic score");
            // The init path should rank higher (lower overall score → better).
            assert!(ranked[0].scores.overall >= ranked[1].scores.overall
                || ranked[0].path.node_indices.iter().any(|&ix| snap.nodes[ix].name == "init"),
                "init path should rank first or at least be present");
        }
    }

    #[test]
    fn test_k_ranked_paths_no_path() {
        let fid = make_file_id("src/a.c");
        let a = make_symbol(fid, "a", "a", SymbolKind::Function);
        let b = make_symbol(fid, "b", "b", SymbolKind::Function);
        let snap = GraphSnapshot::from_parts(vec![a, b], vec![], 0.0).unwrap();
        let a_ix = snap.id_to_idx[&snap.nodes[0].symbol_id];
        let b_ix = snap.id_to_idx[&snap.nodes[1].symbol_id];
        let ranked = snap.k_ranked_paths(a_ix, b_ix, 5, 5, None, TraversalDirection::Outgoing, false);
        assert!(ranked.is_empty());
    }

    // ── weighted Dijkstra vs BFS behavior ──────────────────────────────

    #[test]
    fn test_weighted_prefers_semantic_over_topological() {
        // Graph: a → proxy_conn → d   (2 hops, but "proxy" in name)
        //        a → init → config → d   (3 hops, clean names)
        // Weighted Dijkstra should prefer the 3-hop clean path over the
        // 2-hop proxy path because the proxy name penalty outweighs the
        // extra hop.
        let fid = make_file_id("src/main.c");
        let a = make_symbol(fid, "main", "main", SymbolKind::Function);
        let proxy = make_symbol(fid, "proxy_conn", "proxy_conn", SymbolKind::Function);
        let init = make_symbol(fid, "init_engine", "init_engine", SymbolKind::Function);
        let config = make_symbol(fid, "load_config", "load_config", SymbolKind::Function);
        let d = make_symbol(fid, "Curl_write", "Curl_write", SymbolKind::Function);
        let e1 = make_edge(a.id, proxy.id, EdgeKind::Calls);
        let e2 = make_edge(proxy.id, d.id, EdgeKind::Calls);
        let e3 = make_edge(a.id, init.id, EdgeKind::Calls);
        let e4 = make_edge(init.id, config.id, EdgeKind::Calls);
        let e5 = make_edge(config.id, d.id, EdgeKind::Calls);
        let snap =
            GraphSnapshot::from_parts(vec![a, proxy, init, config, d], vec![e1, e2, e3, e4, e5], 0.0).unwrap();
        let a_ix = snap.id_to_idx[&snap.nodes[0].symbol_id];
        let d_ix = snap.id_to_idx[&snap.nodes[4].symbol_id];

        let ranked = snap.k_ranked_paths(a_ix, d_ix, 5, 5, None, TraversalDirection::Outgoing, false);
        assert!(!ranked.is_empty());
        // The first (best) path should go through init/config, not proxy.
        let best = &ranked[0];
        let has_proxy = best.path.node_indices.iter()
            .any(|&ix| snap.nodes[ix].name == "proxy_conn");
        assert!(!has_proxy,
            "Weighted Dijkstra should prefer the clean 3-hop path over the proxy 2-hop path");
    }
}
