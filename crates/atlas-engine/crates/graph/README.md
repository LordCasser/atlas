# graph

Symbol-level call graph construction and in-memory traversal engine.

## Components

### GraphBuilder

Converts resolved references into symbol-level edges:

```
(ReferenceUse, ResolvedTarget) → RawEdge
```

Edge kinds produced:
- `Calls` — function/method/constructor calls
- `Instantiates` — class/struct instantiation
- `Implements` — interface/trait implementation
- `Extends` — class inheritance
- `References` — general symbol references
- `Contains` — container → child relationships

Uses Rayon for parallel edge creation.

### GraphSnapshot

Immutable in-memory graph snapshot loaded from Store. All graph queries are O(1) lookups or bounded BFS/DFS traversals — no SQLite round-trips.

```rust
GraphEngine::from_store(&store, confidence_threshold)
    └── GraphSnapshot::from_store(store, threshold)
        ├── Load all symbols → NodeId mapping
        ├── Load all edges → adjacency lists
        └── Build NodeIx ↔ SymbolId index
```

### GraphEngine

High-level query API:
| Method | Purpose |
|--------|---------|
| `neighbors(id, config)` | BFS neighbors with edge kind filter |
| `callers(id)` | Incoming call edges |
| `callees(id)` | Outgoing call edges |
| `callgraph(id, depth)` | BFS call graph around a symbol |
| `shortest_path(from, to, max_depth)` | BFS shortest path |
| `impact(id, depth)` | Downstream impact analysis |
