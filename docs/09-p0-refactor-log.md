# P0 Architecture Refactor — Implementation Log

> Schema version: 3 → 4 | Date: 2026-05-20

## Overview

P0 refactor addresses the call/member collision bug, renames `references_v2` → `"references"`,
introduces `SemanticBinder`, and fixes language metadata gaps.

## Changes

### 1. ReferenceId::generate() — kind parameter

**Problem**: `obj.method()` produces two captures (`reference.call` + `reference.field`) with
same byte range + text → same `ReferenceId` → `INSERT OR REPLACE` overwrites one.

**Solution**: Add `kind: ReferenceKind` to `ReferenceId::generate()`. The hash now includes
`kind.as_str()` between `file_id` and `start_byte`.

**Files changed**:
- `src/types/ids.rs` — `ReferenceId::generate()` signature + `kind` in hash
- `src/types/structs.rs` — test update
- `src/extraction/symbol_registry.rs` — pass `reference.kind`
- `src/extraction/languages/*.rs` — all 7 adapters pass `kind`
- `src/db/store.rs` — test updates

### 2. references_v2 → "references" table rename

**Problem**: Legacy `_v2` suffix in table name.

**Solution**: Hard rename with schema version bump to 4. The table name `"references"` is
SQL-quoted everywhere because `REFERENCES` is a SQLite reserved keyword.

**Files changed**:
- `src/db/schema.rs` — DDL, indexes, test assertion, version bump
- `src/db/store.rs` — all SQL (UPDATE, SELECT, INSERT, COUNT), migration

**Migration**: `ALTER TABLE references_v2 RENAME TO "references";` for existing databases.

### 3. SemanticBinder module

**Problem**: `SymbolRegistry` fills `source_symbol` but not `scope_id`. Adapters
inconsistently fill `source_symbol`.

**Solution**: New `src/extraction/semantic_binder.rs` wrapping `SymbolRegistry`:
- `bind_source()` — delegates to `SymbolRegistry::resolve_reference_sources()`
- `bind_scope()` — **NEW**: fills `scope_id` via innermost scope lookup
- `bind_edge_sources()` — delegates to `SymbolRegistry::resolve_edge_sources()`
- `bind_all()` — convenience: source + scope + edge in one call

**Key invariant**: `scope_id` does NOT participate in `ReferenceId` generation,
so no ID regeneration is needed when binding scope.

**Files changed**:
- `src/extraction/semantic_binder.rs` — new file (155 lines + tests)
- `src/extraction/mod.rs` — export `SemanticBinder`
- `src/extraction/extract.rs` — pipeline uses `SemanticBinder::bind_all()`

### 4. Language metadata fixes

**Problem**: Missing TSX/JSX file extensions and ArkTS `.sts` extension.

**Solution**:
- TypeScript globs: add `"**/*.tsx"`, `from_extension()`: add `"tsx"`
- JavaScript globs: add `"**/*.jsx"`, `from_extension()`: add `"jsx"`
- ArkTS `from_extension()`: add `"sts"`

**File changed**: `src/types/enums.rs`

### 5. Callsite collision fix

**Problem**: Same-range `Call` + `FieldAccess` references overwrote each other.

**Solution**: Two-part fix:
1. `ReferenceId` now includes `kind` → different IDs for Call vs FieldAccess
2. Callsite derivation already filters `r.kind == ReferenceKind::Call` only

No code change needed for step 2 — it was already correct.

### 6. Adapter source_symbol reduction

**Problem**: Language adapters manually walked the tree-sitter AST to find enclosing
functions (`find_enclosing_*`), duplicating the work that `SemanticBinder::bind_source()`
already does more accurately via the scope tree.

**Solution**:
- All 6 adapters' `normalize_reference()` now set `source_symbol: None`
- All 6 adapters' `normalize_dataflow()` use a placeholder `SymbolId` + `location` field;
  `SemanticBinder::resolve_edge_sources()` rewrites the source via `location`
- All `find_enclosing_*` helper functions deleted (YAGNI):
  - `find_enclosing_function_id` (TypeScript) — ~60 lines
  - `find_enclosing_function_id_py` (Python) — ~50 lines
  - `find_enclosing_method_id` (Java) — ~50 lines
  - `find_enclosing_function_id_c` (C) — ~45 lines
  - `find_enclosing_function_id_cpp` (C++) — ~90 lines (incl. helpers)
  - `find_enclosing_function_id_cj` (Cangjie) — ~40 lines

**Files changed**:
- `src/extraction/languages/typescript.rs`
- `src/extraction/languages/python.rs`
- `src/extraction/languages/java.rs`
- `src/extraction/languages/c.rs`
- `src/extraction/languages/cpp.rs`
- `src/extraction/languages/cangjie.rs`
- `src/extraction/symbol_registry.rs` — doc comment update

## Architectural Invariants (Preserved)

1. References are **never deleted** — they persist with or without resolved targets
2. All IDs are **deterministic blake3 hashes** — same inputs → same ID
3. `source_symbol` is **resolved by SemanticBinder**, not by language adapters
4. `scope_id` is **bound by SemanticBinder**, not by language adapters
5. All semantic facts carry **confidence and provenance**

## Remaining P0 Work

None — all P0 items complete.

## Data Flow (Post-P0)

```
Source Files
     │
     ▼
[extraction] ─── tree-sitter parse ─── FileFacts
     │              (8 language adapters)    │
     │   adapters produce:                   │
     │     symbols, references (no source),  │
     │     imports, scopes, raw_edges        │
     │                                      ▼
     │   SemanticBinder.bind_all():    [db/Store]
     │     bind_source() → source_symbol   SQLite tables:
     │     bind_scope()  → scope_id        files, symbols, scopes,
     │     bind_edge_sources()             "references", imports,
     │                                      edges, callsites
     │                                      │
     ▼                                      ▼
[resolution] ─── 6-strategy pipeline ─── updates "references"
ReferenceResolver                    creates structural edges
     │                                      │
     ▼                                      ▼
[graph/GraphSnapshot] ──► [graph/GraphEngine]
[search/SearchEngine]  ◄── FTS5 + LIKE + fuzzy
[context/ContextBuilder] ◄── AI context
[mcp/McpServer]        ◄── 12 tools
[sync/SyncEngine]      ◄── incremental sync
```
