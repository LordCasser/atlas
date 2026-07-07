# resolution

Reference resolution layer. Resolves unqualified references to fully-qualified symbol targets.

## Three-stage pipeline

```
ReferenceUse { text: "add", ... }
    │
    ▼
Stage 1: Scope-local exact match
    └─ Search parent scopes for symbol with matching name
    │
    ▼
Stage 2: Import/include resolution
    ├─ Resolve import path (tsconfig path alias, relative path)
    ├─ Find target module
    └─ Match imported name
    │
    ▼
Stage 3: Project-wide fuzzy fallback
    └─ GlobalSymbolIndex (in-memory) → NameMatcher (Levenshtein)
    │
    ▼
ResolvedTarget { symbol_id, confidence, strategy, provenance }
```

## Key components

- **`ReferenceResolver`** — orchestrator, produces `Vec<(ReferenceUse, ResolvedTarget)>`
- **`ImportResolver`** — resolves import/include statements to target symbols
- **`NameMatcher`** — fuzzy name matching with edit-distance scoring
- **`GlobalSymbolIndex`** — in-memory index of all symbols for project-wide search
- **`PathAliasResolver`** — tsconfig paths / jsconfig module alias support

## Design note

Resolution only updates the `"references"` table's `resolved` field. Edge creation is delegated to `GraphBuilder` in the `graph` crate. References are never deleted — unresolved references persist with `resolved = NULL`.
