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
- Capability profiles declare named function/method CFG support for all 14
  languages. PHP branch/loop/switch/elseif, fall-through, numeric
  break/continue, and terminal edges are verified; ArkTS remains explicitly
  limited around ArkUI callback/trailing-block syntax.
- C/C++/JS/TS/ArkTS/PHP and Java colon groups preserve implicit switch
  fall-through; Go preserves explicit `fallthrough`. Go `select` communication
  and default clauses are sibling paths; communication headers stay on the
  dispatch, a select without default has no synthetic skip path, and an empty
  select has no reachable successor. Empty switch/match/select arms retain a
  direct `CaseBranch` path to the next executable body or Join according to the
  language's fall-through rules; they do not create synthetic Statement nodes,
  and identical `(source, target, kind)` paths collapse to one deterministic
  edge. `break` and `continue` are persisted as dedicated CFG edges. Exact
  lexical labels resolve for Java, JS/TS/ArkTS, Go, Rust, and Kotlin and remain
  attached while crossing finally or managed cleanup. C/C++/Go direct
  same-function goto/label pairs use dedicated `Goto` edges; unresolved or
  non-direct targets terminate the local best-effort path. Computed goto,
  PHP/C# goto, C# `goto case/default`, cross-scope C++ destruction, and
  grammar-hidden labels remain explicit boundaries.
- Python unguarded syntax-irrefutable wildcard, capture, `as`, grouping, and OR
  match arms suppress the impossible synthetic no-match path. Rust and Cangjie
  currently do so only for direct unguarded wildcards. Guards remain
  non-exhaustive; Rust/Cangjie binding patterns, Python sequence/mapping/class/
  value patterns, range and type-driven exhaustiveness, and guard/binding
  dataflow are not inferred.
- Rust `?` nodes retain their ordinary success successor and an additional
  residual return-to-Exit continuation. Header expressions on `if`, `match`,
  and loops follow the same rule; `?` inside a nested closure or async block
  belongs to that nested callable boundary and does not exit the outer CFG.
  Rust `let-else` emits separate successful-match and alternative paths;
  explicit return/break/continue and unconditional-loop alternatives remain
  abrupt, including a `?` residual third path from the evaluated value.
  Standalone unqualified builtin `panic!`/`unreachable!`/`todo!`/
  `unimplemented!` macros terminate the local path as Throw nodes.
  Nested-expression macros, macro shadowing/re-exports, custom never-return
  macros, panic unwinding, and `catch_unwind` recovery remain conservative.
  Comment AST extras are never executable CFG Statement nodes.
- JavaScript/TypeScript/ArkTS, Java, C#, PHP, Python, Kotlin, Cangjie, and Ruby
  lower try/catch/except/finally-style regions with path-isolated finally/ensure
  clones. Ruby covers method-body and nested begin/rescue/else/ensure. Normal,
  return, throw, break, and continue continuations cannot cross into one another;
  nested throws resume the enclosing handler path. One try region is capped at
  64 clones and falls back atomically to an opaque Statement when over budget.
  Java try-with-resources, C# using, Python with, Kotlin use, and Ruby block
  resources route normal and abrupt completions through owner-matched,
  path-isolated BlockExit nodes. Any try/finally or managed-resource region is
  capped at 64 clones and falls back atomically when over budget. Ruby
  block-level break/next resumes after the yielding resource call once
  cleanup completes. Cleanup exceptions conservatively retain ordered `Throw`
  continuations into enclosing handlers/finally regions. Java/C#/PHP direct
  object-created explicit throws stop at the first unguarded syntactically exact
  handler; earlier handlers remain alternatives because inheritance is not
  resolved. Resolved/inherited catch selection, thrown variables and guarded or
  implicit exceptions, cleanup exception suppression or replacement and exact
  exception identity, Ruby retry/redo, computed/PHP/C# goto, C++ cross-scope
  destruction on goto, and grammar-hidden labels remain explicit boundaries.
  Managed cleanup effects are emitted in deterministic LIFO order; Java
  try-with-resources runs its owner-matched exits before catch/finally.
  Go defer registration is part of a bounded CFG × runtime-stack expansion:
  distinct registration paths keep distinct continuation identities and normal
  function exits execute owner-matched `BlockExit` nodes in LIFO order through
  persisted `Defer` edges. Nested call arguments keep registration-time
  resource effects. A defer stack that can grow through a loop, or expansion
  beyond 64 clones, falls back atomically to the annotated base CFG;
  panic/recover/Goexit unwinding and complex anonymous deferred bodies remain
  explicit boundaries.

For detailed capability profiles, see `types::LanguageCapabilityProfile`.

ArkTS uses the TypeScript grammar with a byte-length-preserving `struct` to
`class ` parser normalization. This preserves declarative component fields,
methods, scopes, and UI call ownership against the original source ranges.
When ArkUI error recovery swallows declarations, an optional byte-stable
declaration tree recovers symbols and scopes while the primary tree remains the
only source for references, callsites, lexical facts, dataflow, and CFG.
ArkUI trailing-block calls remain best-effort grammar input, so a file may
correctly expose recovered structural facts while retaining `partial` parse status.
ArkTS/TypeScript abstract classes, interface properties/methods, and enum members
use the existing symbol kinds. Decorators are usage references rather than
synthetic definitions. Field source includes decorators, type, and initializer;
those parts do not yet have dedicated structured IR fields.
Query-time tracing separately bridges `AppStorage.set`/`setOrCreate` values to
matching `@StorageProp`/`@StorageLink` field reads. Matching is syntactic; reverse
links, default initialization, and process boundaries are not modeled.

Type symbols use their complete defining scope. This applies to multiline
struct/class/union/interface/trait/enum declarations, including the closing delimiter.
Lazy cache validation recognizes older one-line ranges in supported brace-based languages
and rebuilds the file once even when its indexed content hash is unchanged.
