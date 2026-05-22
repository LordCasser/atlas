# Atlas Extraction (`atlas-extract`)

Source code extraction powered by tree-sitter queries.

## Architecture

```
Source File
    |
    v
LanguageFrontend / LanguageAdapter  ──► tree_sitter::Language
    |                              + queries/*.scm
    v
QueryEngine::run_queries_text()  ──► QueryResults(captures)
    |
    v
extract_file()  ──► queries + binder + callsite/dataflow/CFG builders
    |
    v
FileFacts { symbols, scopes, references, imports, callsites, bindings, dataflow, CFG, diagnostics }
    |
    v
Store::insert_file_facts()  ──► SQLite
```

Extraction is **purely syntactic** — it runs tree-sitter queries on source files and produces `FileFacts`. Resolution and semantic edges happen later in `atlas-resolve`.

## Modules

| File | Purpose |
|------|---------|
| `mod.rs` | Module exports (`LanguageRegistry`, `LanguageAdapter`, `LanguageFrontend`, `QueryEngine`, `extract_file`) |
| `grammar.rs` | `LanguageRegistry` — maps Language → frontend/grammar/adapter |
| `frontend.rs` | Capability-slot wrapper around language adapters |
| `callsite_spec.rs` | Language-specific callsite extraction hooks |
| `languages/mod.rs` | `LanguageAdapter` trait definition |
| `engine.rs` | `QueryEngine` — runs tree-sitter queries against source text |
| `extract.rs` | `extract_file()` — orchestrator: parse → query → normalize → FileFacts |
| `semantic_binder.rs` | Fills source/scope/binding ownership after raw extraction |
| `lexical_binder.rs` | Builds lexical binding definitions and uses where supported |
| `dataflow_builder.rs` | Builds local provenance facts from data nodes and AST ranges |
| `cfg_builder.rs` | Builds function-local CFG facts where supported |

## QueryEngine (`engine.rs`)

```rust
pub struct QueryEngine;

impl QueryEngine {
    pub fn new() -> Self;
    pub fn run_queries(&self, tree: &Tree, source: &[u8],
                        queries: &[(&str, &str)]) -> anyhow::Result<HashMap<&str, Vec<QueryCapture>>>;
    pub fn run_queries_text(&self, source: &str, language: tree_sitter::Language,
                             queries: &[(&str, &str)]) -> anyhow::Result<HashMap<&str, Vec<QueryCapture>>>;
}
```

Each query returns `Vec<QueryCapture>` containing capture name + node byte range + text. Uses `StreamingIterator` (tree-sitter 0.24 API).

## extract_file() Pipeline (`extract.rs`)

```rust
pub fn extract_file(
    frontend: &LanguageFrontend,
    file_id: FileId,
    file_path: &Path,
    source: &str,
    content_hash: &str,
) -> anyhow::Result<FileFacts>
```

Steps:
1. Receive a preselected `LanguageFrontend`, deterministic `FileId`, source path, source text, and content hash
2. Parse source with the frontend's thread-local tree-sitter parser
3. Run per-language queries for definitions, references, imports, scopes, lexical bindings, dataflow, and CFG captures where available
4. Normalize captures through adapter/front-end hooks
5. Build scope, lexical binding, callsite, dataflow, and CFG facts within the current file
6. Apply `SemanticBinder` so source/scope/binding ownership is centralized
7. Assemble `FileFacts`; cross-file resolution is deferred to `resolution`

## LanguageAdapter Trait (`languages/mod.rs`)

```rust
pub trait LanguageAdapter: Send + Sync {
    /// Which language this adapter handles
    fn language(&self) -> Language;

    /// File extensions (without dot, e.g. "ts")
    fn extensions(&self) -> &[&str];

    /// The tree-sitter Language grammar
    fn tree_sitter_language(&self) -> tree_sitter::Language;

    // Query methods (one per fact kind):
    fn definition_query(&self) -> &str;
    fn reference_query(&self) -> &str;
    fn import_query(&self) -> &str;
    fn scope_query(&self) -> &str;
    fn lexical_query(&self) -> &str { "" }
    fn dataflow_builder_query(&self) -> &str { "" }

    // Normalization hooks:
    fn normalize_definition(...) -> Option<SymbolDef>;
    fn normalize_reference(...) -> Option<ReferenceUse>;
    fn normalize_import(...) -> Option<ImportDef>;
    fn normalize_scope(...) -> Option<ScopeDef> { None }
    fn normalize_lexical(...) -> Option<BindingDef> { None }
    fn normalize_dataflow_builder(...) -> (Option<DataNode>, Option<DataFlowEdge>) { (None, None) }
}
```

### Design: Query-Driven, Not Tree-Walker

Each `LanguageAdapter` provides **tree-sitter S-expression queries** that produce capture groups. The generic extraction engine runs these queries and calls normalization hooks to produce typed facts.

This is a departure from the CodeGraph model (which had `extract_nodes(tree)` and `extract_edges(tree, nodes)`). Instead:

- The shared `extract_file()` pipeline runs the selected frontend queries
- Normalization hooks convert query captures into typed facts
- No central `GenericExtractor` — each language owns its extraction logic

## LanguageRegistry (`grammar.rs`)

```rust
pub struct LanguageRegistry {
    adapters: HashMap<Language, Box<dyn LanguageAdapter>>,
    by_extension: HashMap<String, Language>,
}
```

```rust
impl LanguageRegistry {
    pub fn new(languages: &[Language]) -> Self;
    pub fn detect(file_path: &Path) -> Option<Language>;
    pub fn get(language: Language) -> Option<&dyn LanguageAdapter>;
    pub fn get_for_file(file_path: &Path) -> Option<&dyn LanguageAdapter>;
}
```

### MVP Languages (7) + Experimental Opt-In

| Language | Feature Flag | Crate | Adapter Status |
|----------|-------------|-------|----------------|
| TypeScript | `typescript` | `tree-sitter-typescript` | ✅ Implemented |
| JavaScript | `javascript` | `tree-sitter-typescript` | ✅ Implemented |
| Python | `python` | `tree-sitter-python` | ✅ Implemented |
| Java | `java` | `tree-sitter-java` | Implemented with lower trace capability |
| C | `c` | `tree-sitter-c` | Implemented with lower trace capability |
| C++ | `cpp` | `tree-sitter-cpp` | Implemented with lower trace capability |
| ArkTS | `arkts` | `tree-sitter-typescript` | Implemented via TypeScript grammar fallback |
| Cangjie | `cangjie` | `tree-sitter-cangjie` | Experimental opt-in, not in MVP/all-languages |

## Query File Convention

Each language adapter should have its tree-sitter queries in the `queries/` directory:

```
atlas/extraction/queries/
    typescript/
        definitions.scm
        references.scm
        imports.scm
        scopes.scm
        callsites.scm
        dataflow_builder.scm
        cfg.scm
    python/
        ...
    java/
        ...
```

## ExtractError

```rust
pub enum ExtractError {
    Io(std::io::Error),
    Parse(String),
    Query(String),
    Other(String),
}
```

## Cross-Module Contract

1. **Extraction is single-file only** — it produces `FileFacts`; cross-file resolution and graph promotion happen later.
2. **Resolution never deletes facts** — references and symbols persist regardless of resolution status.
3. **Language capability is explicit** — adapters/frontends must report feature support and limitations for trace output.
4. **Adding a language requires**: adapter/front-end hooks + query files + fixtures. No central mega-extractor.
