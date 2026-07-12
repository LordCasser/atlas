# Atlas Core Types

Core type system for the Atlas semantic knowledge graph engine.

## ID Types (`ids.rs`)

| Type | Input | Hash | Size |
|------|-------|------|------|
| `FileId` | `project_relative_path` | blake3 | 32B |
| `SymbolId` | `file_id + language + symbol_path + kind + discriminator` | blake3 | 32B |
| `ScopeId` | `file_id + parent + kind + start_byte` | blake3 | 32B |
| `ReferenceId` | `file_id + kind + source/range + reference_text` | blake3 | 32B |
| `EdgeId` | `source + target + kind + ref_id/provenance` | blake3 | 32B |
| `CallsiteId` | `ref_id + caller + start_byte` | blake3 | 32B |
| `ImportId` | `file_id + kind + module + imported_name + start_byte` | blake3 | 32B |
| `BindingId` | `file_id + scope_id + kind + name + start_byte` | blake3 | 32B |
| `DataNodeId` | `file_id + function_id? + kind + name/access_path + start_byte` | blake3 | 32B |
| `CfgNodeId` | `function_id + kind + start_byte` | blake3 | 32B |

All ID types implement: `Copy`, `Default`, `PartialEq`, `Eq`, `Hash`, `Ord`, `Display` (hex), `FromStr`, `Serialize`, `Deserialize`, `ToSql` (BLOB), `FromSql`.

ID generation uses the `define_id!` macro to eliminate boilerplate.

## Enums (`enums.rs`)

| Enum | Variants | Description |
|------|----------|-------------|
| `Language` | 14 languages | TypeScript, JavaScript, Python, Java, C, Cpp, ArkTS, Go, CSharp, Rust, PHP, Ruby, Kotlin, Cangjie; all are enabled by the default feature set |
| `SymbolKind` | 21 | File, Module, Class, Struct, Interface, Trait, Enum, EnumMember, Function, Method, Property, Field, Variable, Constant, TypeAlias, Namespace, Parameter, Constructor, Macro, Decorator, Package |
| `EdgeKind` | 22 | Contains, Calls, Imports, Includes, Exports, Extends, Implements, References, TypeOf, Returns, Instantiates, Overrides, Decorates, Defines, Argument, Parameter, Assigns, Reads, Writes, FieldRead, FieldWrite, RegistersCallback |
| `ReferenceKind` | 12 | Usage, TypeReference, Call, Import, FieldAccess, Inheritance, Implementation, Override, Decoration, Read, Write, Instantiation |
| `ImportKind` | 6 | Include, Import, FromImport, ExportFrom, Package, Use |
| `ScopeKind` | 13 | File, Module, Class, Struct, Interface, Enum, Function, Method, Block, Loop, Conditional, Namespace, Trait |
| `Visibility` | 5 | Public, Private, Protected, Internal, Package |
| `ResolutionStrategy` | current enum | Exact, scope/import/name/fuzzy/heuristic strategies |
| `Provenance` | current enum | Origin of extracted or inferred facts |
| `ResolutionStatus` | 3 | Resolved, Unresolved, Partial |
| `ParseStatus` | 4 | Success, PartialFailure, Failure, NotParsed |

`Confidence` is a `f32` newtype in `[0.0, 1.0]`.

## Core IR (`structs.rs`)

### TextRange
Byte-offset + line-column point and span for all source locations.

### SymbolDef
```rust
struct SymbolDef {
    id: SymbolId,
    kind: SymbolKind,
    name: String,
    qualified_name: String,
    symbol_path: Vec<String>,
    file_id: FileId,
    language: Language,
    range: TextRange,
    name_range: TextRange,
    signature: Option<String>,
    visibility: Option<Visibility>,
    exported: bool,
    static_: bool,
    async_: bool,
    container: Option<SymbolId>,
    scope_id: Option<ScopeId>,
    package_name: Option<String>,
    namespace_path: Vec<String>,
    layer: String,
}
```

### ReferenceUse
```rust
struct ReferenceUse {
    id: ReferenceId,
    file_id: FileId,
    source_symbol: SymbolId,
    scope_id: Option<ScopeId>,
    kind: ReferenceKind,
    text: String,
    name: String,
    receiver: Option<String>,
    arity: Option<u32>,
    range: TextRange,
    resolved: Option<ResolvedTarget>,  // preserved after resolution!
}
```

### ResolvedTarget
```rust
struct ResolvedTarget {
    symbol_id: SymbolId,
    confidence: Confidence,
    strategy: ResolutionStrategy,
    provenance: Provenance,
}
```

### ScopeDef
```rust
struct ScopeDef {
    id: ScopeId,
    file_id: FileId,
    kind: ScopeKind,
    name: Option<String>,
    scope_path: String,
    range: TextRange,
    parent_id: Option<ScopeId>,
}
```

### ImportDef
```rust
struct ImportDef {
    id: ImportId,
    file_id: FileId,
    kind: ImportKind,
    module: String,
    imported_name: String,
    local_name: Option<String>,
    is_wildcard: bool,
    is_relative: bool,
    range: TextRange,
}
```

Each imported binding is one fact. Module-only facts are reserved for
side-effect imports, bare requires, and wildcard re-exports. `range` covers the
complete statement, and `local_name` is present only for an explicit rename.

### Callsite
```rust
struct Callsite {
    id: CallsiteId,
    reference_id: Option<ReferenceId>,
    caller: SymbolId,
    receiver: Option<String>,
    args: Vec<ArgumentFact>,
    range: TextRange,
    callee_range: Option<TextRange>,
}
```

### RawEdge
```rust
struct RawEdge {
    id: EdgeId,
    source: SymbolId,
    target: SymbolId,
    kind: EdgeKind,
    confidence: Confidence,
    provenance: Provenance,
}
```

### FileFacts
```rust
struct FileFacts {
    file: FileInfo,
    symbols: Vec<SymbolDef>,
    scopes: Vec<ScopeDef>,
    references: Vec<ReferenceUse>,
    imports: Vec<ImportDef>,
    exports: Vec<SymbolId>,
    raw_edges: Vec<RawEdge>,
    callsites: Vec<Callsite>,
    bindings: Vec<BindingDef>,
    binding_uses: Vec<BindingUse>,
    data_nodes: Vec<DataNode>,
    dataflow_edges: Vec<DataFlowEdge>,
    cfg_nodes: Vec<CfgNode>,
    cfg_edges: Vec<CfgEdge>,
    diagnostics: Vec<ExtractDiagnostic>,
    budget_exceeded: bool,
    lexical_failed: bool,
    dataflow_failed: bool,
    cfg_failed: bool,
}
```
The primary extraction output. Extraction writes single-file facts; resolution and graph construction enrich them without deleting unresolved occurrences.

### ExtractDiagnostic
```rust
struct ExtractDiagnostic {
    message: String,
    level: DiagnosticLevel,
    range: Option<TextRange>,
    source: Provenance,
}
```

## Design Principles

1. **Deterministic IDs** — everything hashes from content, not position
2. **Preserved state** — `ReferenceUse.resolved` records resolution result; unresolved refs are NOT deleted
3. **Confidence everywhere** — every semantic edge carries `confidence` and `provenance`
4. **Separation of concerns** — extraction produces facts, resolution produces edges, storage persists both
