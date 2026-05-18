# Atlas Resolution (`atlas-resolve`)

Reference resolution: mapping `ReferenceUse` → `ResolvedTarget` → semantic `RawEdge`.

## Architecture

```
Unresolved references (DB)
        |
        v
ResolutionContext::build() ─── Loads symbols/scopes/imports from Store once
        |
        v
ReferenceResolver::resolve_one() ─── Six-stage pipeline:
  1. Builtin filter (console.log, print, os.path, ...)
  2. Scope-local exact match (walks scope chain)
  3. Container/class-local (method → class scope)
  4. Same-file exact match
  5. Import resolution (candidate qnames → DB → FTS5)
  6. Project-wide fuzzy fallback (FTS5 + Levenshtein)
        |
        v
Store::update_reference_resolution() ─── Updates DB in place
        |
        v
Edge promotion: Calls → Instantiates/Implements
```

## Modules

| Module | Purpose |
|--------|---------|
| `mod.rs` | `ReferenceResolver` — orchestrator with 6-stage pipeline |
| `context.rs` | `ResolutionContext` — in-memory indexes (symbols_by_scope/id/qname, scopes_by_id, scope_parents) |
| `name_matcher.rs` | `NameMatcher` — exact/case-insensitive/Levenshtein scoring + `best_match()` selection |
| `import_resolver.rs` | `ImportResolver` — candidate qname generation + DB/fallback lookup |
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
| 5 | Imported | Follow import paths to candidates | 0.8 |
| 6 | ProjectFuzzy | FTS5 project-wide + Levenshtein | ≥0.6 |

## Edge Promotion

After resolution, certain reference→target pairs get promoted to higher-level edges:

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
4. **Adding a language** requires updating `BuiltinFilter` (builtin set) + `ImportResolver` (if imports differ).
