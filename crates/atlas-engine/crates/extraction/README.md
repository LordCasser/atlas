# extraction

Tree-sitter based code parsing and fact extraction layer.

## Architecture

```
source code
    │
    ▼
tree-sitter parser → CST (Concrete Syntax Tree)
    │
    ▼
LanguageFrontend (slot-based per-language interface)
    ├── SymbolExtractor     → SymbolDef
    ├── ReferenceExtractor  → ReferenceUse
    ├── ScopeExtractor      → ScopeDef
    ├── ImportExtractor     → ImportDef
    ├── CallsiteExtractor   → Callsite
    ├── LexicalBindingSpec  → BindingDef
    └── DataFlowSpec        → DataNode, DataFlowEdge (optional)
    │
    ▼
extract_file_with_mode() → FileFacts
```

## Extraction modes

| Mode | Produced facts | Use |
|------|----------------|-----|
| `Manifest` | Top-level symbols only | Fast candidate inventory (`atlas index --analysis manifest`) |
| `ResolutionSymbols` | All symbols, imports, scopes, scope tree | Internal dependency target preparation |
| `Structural` | Symbols, references, imports, scopes, lexical binding definitions, callsites, exports | Default index (`atlas index`) |
| `LazyDataflow { window }` | Window-local binding uses, dataflow and supported CFG; structural facts are reused | On-demand trace/semantic analysis |
| `Full` | Structural + complete file dataflow + supported CFG | `atlas index --analysis full` |

## Language frontends

Each language implements a `LanguageFrontend` via slot-based composition:

- **14 languages** with DataflowInterproc capability (all compiled by default)
- All languages support full structural analysis and DataflowInterproc tracing.
- Capability profiles currently declare CFG support for 12 languages; ArkTS and PHP are the exceptions.

For detailed capability profiles, see `types::LanguageCapabilityProfile`.

Type symbols use their complete defining scope. This applies to multiline
struct/class/union/interface/trait/enum declarations, including the closing delimiter.
Lazy cache validation recognizes older one-line ranges in supported brace-based languages
and rebuilds the file once even when its indexed content hash is unchanged.
