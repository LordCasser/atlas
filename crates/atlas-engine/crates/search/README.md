# search

FTS5 full-text search and fuzzy matching engine for symbols.

## Architecture

```
SearchEngine
├── FTS5 query (symbols_fts table)
│   └── Fast prefix/term matching via SQLite FTS5
├── LIKE fallback (for exact substring)
│   └── SQL LIKE with pattern escaping
└── Fuzzy prefix matching (Levenshtein distance)
    └── Post-filter results by edit distance
```

## Query format

```
Query text is passed to all three layers. Results are merged and scored:

1. FTS5: relevance scoring from SQLite
2. LIKE: constant score for exact match
3. Fuzzy: Levenshtein distance → inverted score

Final ranking: combined score from all matching layers.
```

## Options

| Parameter | Default | Purpose |
|-----------|---------|---------|
| `query` | (required) | Search text |
| `limit` | 20 | Max results |
| `kind` | None | Filter by `SymbolKind` (function, class, ...) |

## Public API

```rust
SearchEngine::new(store: Arc<Store>, graph: Arc<GraphEngine>)
SearchEngine::search_simple(query, limit)              → Vec<SearchEntry>
SearchEngine::search_by_kind(query, kind, limit)       → Vec<SearchEntry>
SearchEngine::refresh_graph(graph)                     // after external index changes
```
