# Atlas 当前模块契约

> 当前有效模块接口草案。该文档不是最终 API 冻结，但后续实现应遵守这里的边界：query-driven extraction、保留 references、SQLite + GraphSnapshot、MCP-first。

---

## 1. core

### 1.1 IDs

```rust
pub struct FileId([u8; 32]);
pub struct SymbolId([u8; 32]);
pub struct ScopeId([u8; 32]);
pub struct ReferenceId([u8; 32]);
pub struct EdgeId([u8; 32]);
pub struct CallsiteId([u8; 32]);
```

基本要求：

```text
stable within same source facts
deterministic
hex/base64 display
serde support
rusqlite ToSql/FromSql support or wrapper conversion
```

### 1.2 Enums

```rust
pub enum Language {
    C,
    Cpp,
    Python,
    Java,
    ArkTS,
    TypeScript,
    JavaScript,
    Cangjie,
}

pub enum SymbolKind {
    File,
    Module,
    Namespace,
    Package,
    Class,
    Struct,
    Interface,
    Function,
    Method,
    Constructor,
    Destructor,
    Field,
    Property,
    Variable,
    Constant,
    Enum,
    EnumMember,
    TypeAlias,
    Import,
    Export,
}

pub enum ReferenceKind {
    Calls,
    Instantiates,
    References,
    Imports,
    Includes,
    Extends,
    Implements,
    Decorates,
    TypeOf,
    Returns,
}

pub enum EdgeKind {
    Contains,
    Calls,
    Imports,
    Includes,
    Exports,
    Extends,
    Implements,
    References,
    TypeOf,
    Returns,
    Instantiates,
    Overrides,
    Decorates,
    Defines,
    Argument,
    Parameter,
    Assigns,
    Reads,
    Writes,
    FieldRead,
    FieldWrite,
}
```

### 1.3 Ranges

```rust
pub struct TextRange {
    pub start_byte: u32,
    pub end_byte: u32,
    pub start_line: u32,
    pub end_line: u32,
    pub start_col: u32,
    pub end_col: u32,
}
```

---

## 2. IR contracts

### 2.1 FileFacts

```rust
pub struct FileFacts {
    pub file: FileInfo,
    pub symbols: Vec<SymbolDef>,
    pub scopes: Vec<ScopeDef>,
    pub references: Vec<ReferenceUse>,
    pub imports: Vec<ImportDef>,
    pub exports: Vec<ExportDef>,
    pub raw_edges: Vec<RawEdge>,
    pub callsites: Vec<Callsite>,
    pub diagnostics: Vec<ExtractDiagnostic>,
}
```

Invariant：

```text
所有 symbols/references/scopes/callsites 必须属于同一个 file
byte ranges 必须在文件长度内
contains edges 可由 container/scope/range 推断，也可显式 raw_edges 输出
```

### 2.2 SymbolDef

```rust
pub struct SymbolDef {
    pub id: SymbolId,
    pub kind: SymbolKind,
    pub name: String,
    pub qualified_name: String,
    pub symbol_path: String,
    pub file_id: FileId,
    pub language: Language,
    pub range: TextRange,
    pub name_range: TextRange,
    pub signature: Option<String>,
    pub visibility: Option<Visibility>,
    pub exported: bool,
    pub static_: bool,
    pub async_: bool,
    pub container: Option<SymbolId>,
    pub scope_id: ScopeId,
    pub package_name: Option<String>,
    pub namespace_path: Option<String>,
}
```

### 2.3 ScopeDef

```rust
pub struct ScopeDef {
    pub id: ScopeId,
    pub file_id: FileId,
    pub parent: Option<ScopeId>,
    pub owner_symbol: Option<SymbolId>,
    pub kind: ScopeKind,
    pub range: TextRange,
}
```

### 2.4 ReferenceUse

```rust
pub struct ReferenceUse {
    pub id: ReferenceId,
    pub file_id: FileId,
    pub source_symbol: Option<SymbolId>,
    pub scope_id: ScopeId,
    pub kind: ReferenceKind,
    pub text: String,
    pub name: String,
    pub receiver: Option<String>,
    pub arity: Option<u16>,
    pub range: TextRange,
    pub resolved: Option<ResolvedTarget>,
}
```

### 2.5 ImportDef

```rust
pub struct ImportDef {
    pub id: ImportId,
    pub file_id: FileId,
    pub kind: ImportKind,
    pub module: String,
    pub imported_name: Option<String>,
    pub local_name: Option<String>,
    pub is_wildcard: bool,
    pub is_relative: bool,
    pub range: TextRange,
}
```

### 2.6 RawEdge

```rust
pub struct RawEdge {
    pub source: SymbolId,
    pub target: SymbolId,
    pub kind: EdgeKind,
    pub provenance: Provenance,
    pub confidence: f32,
    pub ref_id: Option<ReferenceId>,
    pub metadata: serde_json::Value,
    pub location: Option<TextRange>,
}
```

### 2.7 Callsite

```rust
pub struct Callsite {
    pub id: CallsiteId,
    pub reference_id: ReferenceId,
    pub caller: Option<SymbolId>,
    pub callee: Option<SymbolId>,
    pub receiver: Option<String>,
    pub args: Vec<ArgumentFact>,
    pub range: TextRange,
}
```

---

## 3. languages module

### 3.1 LanguageAdapter

```rust
pub trait LanguageAdapter: Send + Sync {
    fn language(&self) -> Language;
    fn extensions(&self) -> &'static [&'static str];
    fn tree_sitter_language(&self) -> tree_sitter::Language;

    fn definition_query(&self) -> &'static str;
    fn reference_query(&self) -> &'static str;
    fn import_query(&self) -> &'static str;
    fn scope_query(&self) -> Option<&'static str> { None }

    fn normalize_definition(
        &self,
        captures: CaptureSet<'_>,
        source: &SourceFile,
    ) -> Option<SymbolDef>;

    fn normalize_reference(
        &self,
        captures: CaptureSet<'_>,
        source: &SourceFile,
    ) -> Option<ReferenceUse>;

    fn normalize_import(
        &self,
        captures: CaptureSet<'_>,
        source: &SourceFile,
    ) -> Option<ImportDef>;

    fn language_specific_facts(
        &self,
        _tree: &tree_sitter::Tree,
        _source: &SourceFile,
    ) -> Vec<LanguageFact> {
        Vec::new()
    }
}
```

### 3.2 Adapter registry

```rust
pub struct LanguageRegistry {
    adapters: HashMap<Language, Arc<dyn LanguageAdapter>>,
    extension_map: HashMap<&'static str, Language>,
}
```

Required adapters:

```text
CAdapter
CppAdapter
PythonAdapter
JavaAdapter
TypeScriptAdapter
JavaScriptAdapter
ArkTSAdapter
CangjieAdapter
```

---

## 4. extract module

### 4.1 Parser service

```rust
pub struct ParserService {
    registry: Arc<LanguageRegistry>,
}

impl ParserService {
    pub fn parse_file(&self, source: &SourceFile) -> Result<ParsedFile>;
}
```

### 4.2 QueryEngine

```rust
pub struct QueryEngine;

impl QueryEngine {
    pub fn extract(&self, parsed: &ParsedFile, adapter: &dyn LanguageAdapter) -> Result<FileFacts>;
}
```

### 4.3 ExtractionOrchestrator

```rust
pub struct ExtractionOrchestrator {
    registry: Arc<LanguageRegistry>,
    store: Arc<Store>,
    config: ExtractionConfig,
}

impl ExtractionOrchestrator {
    pub fn index_project(&self, root: &Path) -> Result<IndexStats>;
    pub fn index_files(&self, files: &[PathBuf]) -> Result<IndexStats>;
    pub fn parse_to_facts(&self, path: &Path) -> Result<FileFacts>;
}
```

---

## 5. store module

### 5.1 Store API

```rust
pub struct Store {
    writer: StoreWriter,
    reader: StoreReader,
}
```

### 5.2 Writer

```rust
impl StoreWriter {
    pub fn initialize_schema(&self) -> Result<()>;
    pub fn write_file_facts(&self, facts: &FileFacts) -> Result<()>;
    pub fn delete_file_facts(&self, file_id: FileId) -> Result<()>;
    pub fn write_resolutions(&self, resolutions: &[Resolution]) -> Result<()>;
    pub fn write_edges(&self, edges: &[ResolvedEdge]) -> Result<()>;
}
```

### 5.3 Reader

```rust
impl StoreReader {
    pub fn get_file(&self, file_id: FileId) -> Result<Option<FileRecord>>;
    pub fn get_symbol(&self, symbol_id: SymbolId) -> Result<Option<SymbolRecord>>;
    pub fn search_symbols(&self, query: &SearchQuery) -> Result<Vec<ScoredSymbol>>;
    pub fn get_references_to(&self, symbol_id: SymbolId) -> Result<Vec<ReferenceRecord>>;
    pub fn load_graph_snapshot(&self, config: SnapshotConfig) -> Result<GraphSnapshot>;
    pub fn get_stats(&self) -> Result<StoreStats>;
}

pub struct StoreStats {
    pub total_files: i64,
    pub total_symbols: i64,
    pub total_edges: i64,
    pub total_references: i64,
    pub unresolved_references: i64,
    pub sqlite_version: String,
    pub symbols_by_kind: Vec<(String, i64)>,     // e.g. {"class": 42, "function": 128}
    pub files_by_language: Vec<(String, i64)>,    // e.g. {"typescript": 50, "python": 12}
}
```

---

## 6. resolve module

### 6.1 Resolver

```rust
pub struct ReferenceResolver {
    store: Arc<Store>,
    config: ResolutionConfig,
}

impl ReferenceResolver {
    pub fn resolve_all(&self) -> Result<ResolutionStats>;
    pub fn resolve_files(&self, files: &[FileId]) -> Result<ResolutionStats>;
    pub fn resolve_one(&self, reference: &ReferenceRecord, ctx: &ResolutionContext) -> Resolution;
}
```

### 6.2 ResolutionContext

```rust
pub struct ResolutionContext {
    pub file: FileRecord,
    pub symbols_in_file: Vec<SymbolRecord>,
    pub imports: Vec<ImportRecord>,
    pub scopes: Vec<ScopeRecord>,
    pub indexes: ResolutionIndexes,
}
```

### 6.3 Strategy ordering

```text
builtin/external filter
scope-local exact
container/class-local
same-file exact
import/include/package
same namespace/package
framework hook optional
project-wide exact + proximity
fuzzy fallback
```

---

## 7. graph module

### 7.1 GraphSnapshot

```rust
pub struct GraphSnapshot {
    pub nodes: Vec<NodeSummary>,
    pub edges: Vec<EdgeSummary>,
    pub id_to_idx: HashMap<SymbolId, NodeIx>,
    pub name_index: HashMap<String, Vec<NodeIx>>,
    pub qname_index: HashMap<String, Vec<NodeIx>>,
    pub file_index: HashMap<FileId, Vec<NodeIx>>,
    pub outgoing: Vec<Vec<EdgeIx>>,
    pub incoming: Vec<Vec<EdgeIx>>,
}
```

### 7.2 GraphEngine

```rust
pub struct GraphEngine {
    snapshot: Arc<GraphSnapshot>,
}

impl GraphEngine {
    pub fn neighbors(&self, id: SymbolId, config: TraversalConfig) -> Subgraph;
    pub fn callers(&self, id: SymbolId, depth: u8) -> CallGraph;
    pub fn callees(&self, id: SymbolId, depth: u8) -> CallGraph;
    pub fn callgraph(&self, id: SymbolId, depth: u8) -> CallGraph;
    pub fn impact(&self, id: SymbolId, depth: u8) -> Subgraph;
    pub fn shortest_path(&self, from: SymbolId, to: SymbolId, config: PathConfig) -> Option<GraphPath>;
}
```

---

## 8. search/context module

### 8.1 SearchEngine

```rust
pub struct SearchEngine {
    store: Arc<Store>,
    graph: Arc<GraphEngine>,
}

impl SearchEngine {
    pub fn search(&self, query: &str, limit: usize, options: &SearchOptions) -> Result<Vec<SearchResult>>;
    pub fn search_simple(&self, query: &str, limit: usize) -> Result<Vec<SearchResult>>;
    pub fn search_by_kind(&self, query: &str, kind: SymbolKind, limit: usize) -> Result<Vec<SearchResult>>;
    pub fn search_in_file(&self, query: &str, file_id: &FileId, limit: usize) -> Result<Vec<SearchResult>>;
    pub fn fuzzy_search(&self, name: &str, language: Option<Language>, limit: usize) -> Result<Vec<(SymbolDef, f64)>>;
}

pub struct SearchOptions {
    pub language: Option<Language>,
    pub file_path_pattern: Option<String>,  // matches real file path, not FileId hex
    pub kind_filter: Option<SymbolKind>,
    pub min_confidence: Option<f64>,
}

pub struct SearchResult {
    pub symbol: SymbolDef,
    pub score: SearchScore,
    pub matched_field: String,
    pub snippet: Option<String>,
    pub file_path: Option<String>,  // resolved FileId → human-readable path
}

pub struct SearchScore {
    pub fts_score: f64,     // IDF-weighted
    pub graph_score: f64,   // degree-based centrality
    pub name_score: f64,    // exact/camelCase/Levenshtein similarity
    pub kind_bonus: f64,    // class > function > variable
    pub path_bonus: f64,    // query in qualified_name
    pub total: f64,         // weighted sum
}
```

### 8.2 ContextBuilder

```rust
pub struct ContextBuilder {
    store: Arc<Store>,
    graph: Arc<GraphEngine>,
}

impl ContextBuilder {
    pub fn build_context_for_symbol(&self, symbol_id: &SymbolId) -> Result<ContextView>;
    pub fn build_context_slice(&self, symbol_id: &SymbolId) -> Result<ContextSlice>;
    pub fn build_context_for_query(&self, query: &str) -> Result<Option<ContextView>>;
}

pub struct ContextView {
    pub subject: SymbolDef,
    pub callers: Vec<SymbolDef>,
    pub callees: Vec<SymbolDef>,
    pub file_peers: Vec<SymbolDef>,
    pub importers: Vec<String>,
    pub dependencies: Vec<String>,
}

impl ContextView {
    pub fn to_markdown(&self) -> String;
}
```

---

## 9. sync module

```rust
pub struct SyncEngine {
    extractor: Arc<ExtractionOrchestrator>,
    resolver: Arc<ReferenceResolver>,
    graph_manager: Arc<GraphSnapshotManager>,
}

impl SyncEngine {
    pub fn sync(&self, root: &Path) -> Result<SyncStats>;
    pub fn changed_files(&self, root: &Path) -> Result<ChangedFiles>;
}
```

---

## 10. mcp module

### 10.1 Server

```rust
pub struct McpServer {
    project_manager: ProjectManager,
    tools: ToolRouter,
}

impl McpServer {
    pub async fn run_stdio(self) -> Result<()>;
}
```

### 10.2 Tools

Required tools:

```text
atlas_status
atlas_files
atlas_search
atlas_symbol
atlas_neighbors
atlas_callers
atlas_callees
atlas_callgraph
atlas_impact
atlas_path
atlas_context
atlas_explore
```

### 10.3 Tool output requirement

Every graph-related tool should include:

```text
summary
nodes
edges/references
confidence/provenance when applicable
code snippets optional
truncation warning if output budget hit
```

---

## 11. cli module

```rust
atlas init [--project <path>]
atlas index [--project <path>] [--include <glob>] [--exclude <glob>]
atlas sync [--project <path>]
atlas search <query> [--project <path>] [--limit <n>] [--kind <kind>] [--json]
atlas context <query> [--project <path>]
atlas status [--project <path>]
atlas files [--project <path>]
atlas mcp [--project <path>]
atlas doctor [--project <path>]
```

### 11.1 search

Three-tier fallback search strategy:
1. **FTS5 full-text** (BM25, prefix matching via `symbols_fts` virtual table)
2. **LIKE substring** (when FTS5 returns nothing)
3. **Levenshtein edit distance** fuzzy match (final fallback for typos, ~40% tolerance)

Multi-signal scoring: `BM25/IDF(0.40) + graph_degree(0.20) + name_similarity(0.25) + kind_bonus(0.10) + path_relevance(bonus)`

Options:
- `--kind <kind>`: Filter by symbol kind (class, function, method, variable, etc.)
- `--json`: Output results as JSON array with name/kind/qualified_name/file_path/line/signature/score
- `--limit <n>`: Maximum results (default 10)

### 11.2 context

Builds AI context around a symbol: callers, callees, file peers, importers, dependencies.
Outputs Markdown via `ContextView::to_markdown()`.

### 11.3 files

Lists all indexed files with language annotation and summary statistics.

---

## 12. Cross-module invariants

1. Extraction never writes directly to final graph edges except structural/raw facts; resolution owns semantic edges.
2. References are never deleted simply because they resolved.
3. All non-structural semantic edges must carry confidence/provenance.
4. MCP must validate project paths.
5. GraphSnapshot is immutable once published.
6. Sync writes through transactions and refreshes snapshot only after successful commit.
7. Adding a language must not require editing a central mega-extractor.
