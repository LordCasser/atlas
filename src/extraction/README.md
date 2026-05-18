# Atlas Extraction (`atlas-extract`)

Source code extraction powered by tree-sitter queries.

## Architecture

```
Source File
    |
    v
LanguageAdapter::parse()
    |
    v
FileFacts { symbols, scopes, references, imports, ..., raw_edges }
```

Extraction is **purely syntactic** — it runs tree-sitter queries on source files and produces `FileFacts`. Resolution and semantic edges happen later in `atlas-resolve`.

## LanguageAdapter Trait (`languages/mod.rs`)

```rust
pub trait LanguageAdapter: Send + Sync {
    /// Which language this adapter handles
    fn language(&self) -> Language;

    /// File extensions (with dot, e.g. ".ts")
    fn extensions(&self) -> &[&str];

    /// The tree-sitter Language grammar
    fn tree_sitter_language(&self) -> tree_sitter::Language;

    /// Parse a source file, returning all extraction facts
    fn parse(&self, file_id: FileId, path: &Path,
             content: &str) -> Result<FileFacts, ExtractError>;

    // Query methods (one per fact kind):
    fn definition_query(&self) -> &str;
    fn reference_query(&self) -> &str;
    fn import_query(&self) -> &str;
    fn scope_query(&self) -> &str;
    fn callsite_query(&self) -> &str;

    // Normalization hooks:
    fn normalize_definition(...) -> Option<SymbolDef>;
    fn normalize_reference(...) -> Option<ReferenceUse>;
    fn normalize_import(...) -> Option<ImportDef>;
}
```

### Design: Query-Driven, Not Tree-Walker

Each `LanguageAdapter` provides **tree-sitter S-expression queries** that produce capture groups. The generic extraction engine runs these queries and calls normalization hooks to produce typed facts.

This is a departure from the CodeGraph model (which had `extract_nodes(tree)'', `extract_edges(tree, nodes)`). Instead:

- One `parse()` call runs all queries at once
- Normalization hooks convert query captures into `FileFacts`
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

### MVP Languages (8)

| Language | Feature Flag | Crate | Adapter Status |
|----------|-------------|-------|----------------|
| TypeScript | `typescript` | `tree-sitter-typescript` | Not implemented |
| JavaScript | `javascript` | `tree-sitter-typescript` | Not implemented |
| Python | `python` | `tree-sitter-python` | Not implemented |
| Java | `java` | `tree-sitter-java` | Not implemented |
| C | `c` | `tree-sitter-c` | Not implemented |
| C++ | `cpp` | `tree-sitter-cpp` | Not implemented |
| ArkTS | `arkts` | `tree-sitter-typescript` | Not implemented |
| Cangjie | `cangjie` | `tree-sitter-cangjie` | Not implemented |

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

1. **Extraction never writes edges** — only produces `FileFacts` with `raw_edges` (calls, contains, exports). Resolution is responsible for semantic edges.
2. **Resolution never deletes facts** — references and symbols persist regardless of resolution status.
3. **Adding a language requires**: a `LanguageAdapter` implementation + query files. No changes to the core engine.
