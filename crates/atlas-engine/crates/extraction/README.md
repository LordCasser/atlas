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

- **14 languages** with DataflowFull capability (all compiled by default)
- All languages support full structural analysis and DataflowFull tracing.
- Capability profiles currently declare CFG support for 12 languages; ArkTS and PHP are the exceptions.

For detailed capability profiles, see `types::LanguageCapabilityProfile`.

C/C++ type symbols use their complete defining scope. This applies to multiline
struct/class/union/enum declarations, including the closing delimiter. Lazy cache
validation treats older one-line ranges as stale and rebuilds the file once even when its
content hash is unchanged.
