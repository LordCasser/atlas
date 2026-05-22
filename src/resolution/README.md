# Atlas Resolution (`atlas-resolve`)

Reference resolution: mapping `ReferenceUse` → resolved reference fields. Symbol-level
edge creation happens later in `GraphBuilder`.

## Architecture

```
Unresolved references (DB)
        |
        v
ResolutionContext::build() ─── Loads symbols/scopes/imports from Store once
        |
        v
ReferenceResolver::resolve_one() ─── Current main pipeline:
  1. Builtin filter (console.log, print, os.path, ...)
  2. Scope-local exact match (walks scope chain)
  3. Container/class-local (method → class scope)
  4. Same-file exact match
  5. Import/include-aware candidate resolution where available
  6. Project-wide exact/proximity/fuzzy fallback
        |
        v
Store::update_reference_resolution() ─── Updates DB in place
        |
        v
GraphBuilder promotes resolved references into symbol_edges
```

## Modules

| Module | Purpose |
|--------|---------|
| `mod.rs` | `ReferenceResolver` — orchestrator with 6-stage pipeline |
| `context.rs` | `ResolutionContext` — in-memory indexes (symbols_by_scope/id/qname, scopes_by_id, scope_parents) |
| `name_matcher.rs` | `NameMatcher` — exact/case-insensitive/Levenshtein scoring + `best_match()` selection |
| `import_resolver.rs` | `ImportResolver` — candidate qname generation + DB/fallback lookup |
| `path_alias.rs` | `PathAliasResolver` — helper for baseUrl/paths-style aliases; wired into ReferenceResolver via `with_path_alias()` when tsconfig.json is present |
| `include_graph.rs` | `IncludeGraph` — C/C++ local include indexing and system include filtering |
| `builtins.rs` | `BuiltinFilter` — per-language builtin sets (TS/JS 55+ items, Python 40+ items) |
| `frameworks/` | Framework-specific resolvers (React, future: Angular, Django, etc.) |

## Resolution Strategies

Priority-ordered:

| # | Strategy | Description | Confidence |
|---|----------|-------------|------------|
| 1 | Builtin | Known language/stdlib built-in | 1.0 |
| 2 | ScopeLocal | Same scope or parent scope match | 1.0 |
| 3 | ClassLocal | Method → containing class lookup | 1.0 |
| 4 | SameFile | Same file, any scope | Depends on match |
| 5 | Imported/Included | Follow import/include facts to candidates where available | ~0.8 |
| 6 | ProjectFallback | Project-wide exact/proximity/fuzzy matching | ≥0.6 |

`PathAliasResolver` is wired into the main resolver via `ReferenceResolver::with_path_alias()`.
Both `atlas index` and the sync engine attempt to load `tsconfig.json` from the project root.
When no tsconfig is found, path aliasing is a no-op.

## Graph Promotion

After resolution, `GraphBuilder` promotes reference→target pairs to symbol-level edges:

| Reference Kind | Target Kind | Promoted Edge |
|----------------|-------------|---------------|
| `Call` | `Class`/`Struct` | `Instantiates` |
| `Call` | `Interface`/`Trait` | `Implements` |

## ResolutionContext

```rust
pub struct ResolutionContext {
    pub file: FileInfo,
    pub symbols: Vec<SymbolDef>,        // All symbols in file
    pub scopes: Vec<ScopeDef>,          // All scopes in file
    pub imports: Vec<ImportDef>,        // All imports in file

    // Indexes
    pub symbols_by_scope: HashMap<ScopeId, Vec<SymbolDef>>,
    pub symbols_by_id: HashMap<SymbolId, SymbolDef>,
    pub symbols_by_qname: HashMap<String, Vec<SymbolDef>>,
    pub scopes_by_id: HashMap<ScopeId, ScopeDef>,
    pub scope_parents: HashMap<ScopeId, ScopeId>,
}
```

## Cross-Module Contract

1. **References are NEVER deleted** — resolution updates `resolved` in place; unresolved refs stay.
2. **All semantic edges carry confidence/provenance** — even Heuristic matches have explicit metadata.
3. **Resolution runs after extraction** — depends on `FileFacts` being stored first.
4. **GraphBuilder owns semantic edges** — resolution must not directly create final graph edges.
5. **Adding a language** requires updating `BuiltinFilter` (builtin set) + `ImportResolver` (if imports differ).
