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

| Mode | Structural facts | Dataflow | CFG | Use |
|------|-----------------|----------|-----|-----|
| `Structural` | ✓ | ✗ | ✗ | Default index (`atlas index`) |
| `Full` | ✓ | ✓ | ✓ | Complete analysis (`atlas index --analysis full`) |
| `LazyDataflow { window }` | ✗ (reuse) | Window only | Window only | On-demand (`trace_variable`) |

## Language frontends

Each language implements a `LanguageFrontend` via slot-based composition:

- **14 languages** with DataflowFull capability (all compiled by default)
- All languages support full structural analysis (symbols, references, call graph) and dataflow tracing

For detailed capability profiles, see `types::LanguageCapabilityProfile`.
