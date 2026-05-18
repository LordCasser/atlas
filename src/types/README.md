# Atlas Core Types (`atlas-core`)

Core type system for the Atlas semantic knowledge graph engine.

## ID Types (`ids.rs`)

| Type | Input | Hash | Size |
|------|-------|------|------|
| `FileId` | `project_relative_path` | blake3 | 32B |
| `SymbolId` | `file_id + language + symbol_path + kind + discriminator` | blake3 | 32B |
| `ScopeId` | `file_id + parent + kind + start_byte` | blake3 | 32B |
| `ReferenceId` | `file_id + source_symbol + byte_range + text` | blake3 | 32B |
| `EdgeId` | `source + target + kind + ref_id/provenance` | blake3 | 32B |
| `CallsiteId` | `ref_id + caller + start_byte` | blake3 | 32B |
| `ImportId` | `file_id + kind + module + imported_name + start_byte` | blake3 | 32B |

All ID types implement: `Copy`, `Default`, `PartialEq`, `Eq`, `Hash`, `Ord`, `Display` (hex), `FromStr`, `Serialize`, `Deserialize`, `ToSql` (BLOB), `FromSql`.

ID generation uses the `define_id!` macro to eliminate boilerplate.

## Enums (`enums.rs`)

| Enum | Variants | Description |
|------|----------|-------------|
| `Language` | 8 | TypeScript, JavaScript, Python, Java, C, Cpp, ArkTS, Cangjie |
| `SymbolKind` | 20 | File, Module, Class, Struct, Interface, Trait, Enum, EnumMember, Function, Method, Property, Field, Variable, Constant, TypeAlias, Namespace, Parameter, Constructor, Macro, Decorator, Package |
| `EdgeKind` | 21 | Contains, Calls, Imports, Includes, Exports, Extends, Implements, References, TypeOf, Returns, Instantiates, Overrides, Decorates, Defines, Argument, Parameter, Assigns, Reads, Writes, FieldRead, FieldWrite |
| `ReferenceKind` | 10 | Usage, TypeReference, Call, Import, FieldAccess, Inheritance, Implementation, Override, Decoration, Read, Write, Instantiation |
| `ImportKind` | 5 | Include, Import, FromImport, Package, Use |
| `ScopeKind` | 13 | File, Module, Class, Struct, Interface, Enum, Function, Method, Block, Loop, Conditional, Namespace, Trait |
| `Visibility` | 5 | Public, Private, Protected, Internal, Package |
| `ResolutionStrategy` | 5 | ExactMatch, NameOnly, FuzzyMatch, Heuristic, Builtin |
| `Provenance` | 3 | TreeSitter, Scip, Heuristic |
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
    symbol_path: String,
    file_id: FileId,
    language: Language,
    range: TextRange,
    name_range: TextRange,
    signature: Option<String>,
    visibility: Visibility,
    exported: bool,
    static_: bool,
    async_: bool,
    container: Option<String>,
    scope_id: Option<ScopeId>,
    package_name: Option<String>,
    namespace_path: Option<Vec<String>>,
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

### Callsite
```rust
struct Callsite {
    id: CallsiteId,
    reference_id: ReferenceId,
    caller: SymbolId,
    callee: Option<SymbolId>,
    receiver: Option<String>,
    args: Vec<String>,
    range: TextRange,
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
    exports: Vec<String>,
    raw_edges: Vec<RawEdge>,
    callsites: Vec<Callsite>,
    diagnostics: Vec<ExtractDiagnostic>,
}
```
The primary extraction output. Extraction writes facts; resolution adds edges; neither deletes facts.

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
