# Search Module

Three-tier fallback search engine with multi-signal scoring.

## Architecture

```
User Query
    │
    ▼
┌─────────────────────────────────────┐
│ Stage 1: FTS5 Full-Text Search     │  BM25 prefix matching via symbols_fts
│   SELECT ... JOIN symbols_fts       │  Appends "*" for prefix matching
│   WHERE MATCH 'query*'              │
└──────────────┬──────────────────────┘
               │ (if empty)
               ▼
┌─────────────────────────────────────┐
│ Stage 2: LIKE Substring Search     │  %query% on name + qualified_name
│   WHERE name LIKE '%query%'         │  Optional language filter
└──────────────┬──────────────────────┘
               │ (if empty)
               ▼
┌─────────────────────────────────────┐
│ Stage 3: Levenshtein Fuzzy Match   │  Load all symbols, compute edit distance
│   max_dist = len(query) * 0.4       │  Both original + snake_case forms
│   Sort by distance, take top N      │
└─────────────────────────────────────┘
```

## Multi-Signal Scoring

```
total = fts_score * 0.40
      + graph_score * 0.20
      + name_score * 0.25
      + kind_bonus * 0.10
      + path_bonus
```

| Signal | Weight | Source |
|--------|--------|--------|
| fts_score | 0.40 | IDF-weighted from FTS5 match count |
| graph_score | 0.20 | Degree centrality (normalized by max degree) |
| name_score | 0.25 | Exact(1.0) > case-insensitive(0.9) > camelCase(0.85) > word overlap(0.5-0.75) > Levenshtein(0-0.7) |
| kind_bonus | 0.10 | Class(0.8) > Function(0.6) > Struct(0.5) > Module(0.4) > Variable(0.3) > Parameter(0.15) |
| path_bonus | +0.15 | Query appears in qualified_name |

## SearchResult

```rust
pub struct SearchResult {
    pub symbol: SymbolDef,         // matched symbol
    pub score: SearchScore,        // multi-signal score
    pub matched_field: String,     // "name" (LIKE) | "fuzzy" (Levenshtein) | "" (FTS5)
    pub snippet: Option<String>,   // reserved for FTS5 snippet()
    pub file_path: Option<String>, // resolved FileId -> human-readable path
}
```

## Post-Filters

Applied after scoring and sorting:
- `file_path_pattern`: matches real file path (case-insensitive substring)
- `kind_filter`: matches `SymbolKind` exactly
- `min_confidence`: minimum `score.total` threshold

## CLI Usage

```bash
# Basic search
atlas search "UserManager"

# Filter by kind
atlas search "spider" --kind class

# JSON output
atlas search "spider" --json

# Combined
atlas search "spider" --kind function --json --limit 20
```

## Files

| File | Purpose |
|------|---------|
| `mod.rs` | SearchEngine, SearchOptions, SearchResult, 3-tier pipeline |
| `scoring.rs` | SearchScore, kind_weight(), idf_weight(), normalize_fts_score() |
| `fts.rs` | FtsQuery builder, escape_fts5(), sanitize_fts5_query() |
| `fuzzy.rs` | Re-exports levenshtein() from types module |
