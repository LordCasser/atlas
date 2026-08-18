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
  attached while crossing finally or managed cleanup. C/C++/Go/C#/PHP direct
  same-function goto/label pairs use dedicated `Goto` edges. C# goto exits run
  intervening using `BlockExit` and path-isolated finally clones from inner to
  outer before the final `Goto` edge; jumps into nested lexical/cleanup regions
  and jumps out of a finally clause are rejected. PHP labels are standalone
  `Join` targets; goto exits run intervening path-isolated finally clones from
  inner to outer, entry into loop/switch and crossing either direction over a
  finally-clause boundary are rejected, while entry into an ordinary block is
  allowed. Unresolved or non-direct targets terminate the local best-effort
  path. Computed goto, C# `goto case/default`, cross-scope C++ destruction, and
  grammar-hidden labels remain explicit boundaries.
- Go mixed short declarations reuse source-earlier bindings in the same block,
  including the function-body parameter exception, while new names activate
  after the declaration and nested switch/select clauses remain isolated.
  Blank identifiers do not create binding/dataflow facts. Type-switch aliases
  are distinct bindings in each case/default clause's implicit block; the guard
  value flows conservatively to every alias and the guard alias token is not a
  read. Identifier-only select receive `:=` targets are clause-local
  declarations, while `=` targets reuse existing bindings; the whole receive
  operation flows to every supported target at confidence 0.78. Case-type
  projection, function-literal ownership, exact receive-result components,
  non-identifier receive targets, and parallel-assignment evaluation order
  remain conservative.
- TypeScript, JavaScript, and ArkTS object/array assignment destructuring
  reuses source-visible identifier bindings and creates no declarations. The
  whole RHS reaches simple, renamed, nested, default-left, and rest identifier
  targets through aggregate `Assign` at confidence 0.85; computed keys and
  default RHS expressions remain reads. Exact property/index projection,
  default activation, nested member/subscript targets, and parallel assignment
  evaluation order remain conservative.
- Java direct and record-pattern captures are bindings in supported
  `if`-condition `instanceof` expressions and Java 21 arrow `switch_rule`
  scopes. Same-named captures in sibling rules retain distinct identities; the
  tested value or switch selector flows conservatively to every supported
  capture at confidence 0.75. Colon-style switch groups, standalone or other
  flow-sensitive boolean contexts, exact record-component projection,
  compiler definite-assignment, and guard control dependencies remain
  conservative.
- Cangjie simple, nested-tuple, and enum-payload `for-in` captures are
  loop-scoped bindings; enum constructor syntax is excluded. The iterable
  provides conservative aggregate provenance to every capture, guard/body uses
  share those identities, and outer same-name bindings become visible again
  after the loop. Exact iterator element/structural projection, compiler
  validation of pattern irrefutability, other tuple/destructuring, and resource
  bindings remain explicit boundaries.
- PHP `[]` and `list()` assignment/`foreach` destructuring produces callable-
  scoped bindings for nested, keyed, and by-reference variable targets. Key
  expressions remain reads. The whole RHS or collection flows conservatively
  to each supported target; exact key/index projection, missing-key/null
  behavior, reference-alias semantics, and dynamic/non-variable targets remain
  explicit boundaries.
- PHP direct file/function/method variable `op=` and prefix/postfix `++`/`--`
  preserve aggregate read-modify-write provenance: the previous value and any
  explicit RHS feed a mutation Expr, then the Expr flows to the coalesced Local
  write at confidence 0.90. Dynamic/non-variable mutation targets, conditional
  `??=` execution, and prefix/postfix result timing remain conservative.
- Ruby local targets in flat, nested, and rest multiple assignment participate
  in the same source-ordered method/module/class/block namespace as simple
  assignments. Explicit RHS lists map by top-level position; a single aggregate
  RHS and nested/rest targets keep conservative group/slice flow. Structural
  element projection, `to_ary` coercion, implicit `nil` fill, parallel
  evaluation order, and numbered parameters remain explicit boundaries.
- Python unguarded syntax-irrefutable wildcard, capture, `as`, grouping, and OR
  match arms suppress the impossible synthetic no-match path. Capture, `as`,
  and star/rest identifiers share the enclosing Python namespace and receive
  conservative subject-to-binding dataflow through guard/body uses. Ordinary
  statement blocks do not create Python lexical scopes; comprehensions remain
  isolated. Rust and Cangjie currently recognize only direct unguarded
  wildcards. Guards remain non-exhaustive; Rust/Cangjie binding patterns,
  Python structural projection/post-match path-definedness, range and
  type-driven exhaustiveness are not inferred.
- Ruby `case ... in` lowers `in_clause` siblings without fall-through. A
  refutable case with no `else` emits the language-required implicit Throw
  path; direct unguarded capture/wildcard patterns suppress it. Pattern
  binding/deconstruction dataflow remains conservative. Python/Rust/Kotlin/
  Cangjie/Ruby sibling constructs propagate `break` to their enclosing loop
  instead of consuming it as a switch exit.
- Rust `?` nodes retain their ordinary success successor and an additional
  residual return-to-Exit continuation. Header expressions on `if`, `match`,
  and loops follow the same rule; `?` inside a nested closure or async block
  belongs to that nested callable boundary and does not exit the outer CFG.
  Rust `let-else` emits separate successful-match and alternative paths;
  explicit return/break/continue and unconditional-loop alternatives remain
  abrupt, including a `?` residual third path from the evaluated value.
  Standalone unqualified builtin `panic!`/`unreachable!`/`todo!`/
  `unimplemented!` macros terminate the local path as Throw nodes.
- A guarded Rust `match` arm persists its guard as an explicit Branch. The
  match dispatch enters the guard through `CaseBranch`; `TrueBranch` enters the
  arm body, while `FalseBranch` ends that guarded-arm path at the shared Join.
  Later arms remain independent sibling paths from the match dispatch. This
  does not prove ordered pattern re-dispatch or pattern predicates, and
  variable Trace does not infer guard-to-value control dependency.
  Nested-expression macros, macro shadowing/re-exports, custom never-return
  macros, panic unwinding, and `catch_unwind` recovery remain conservative.
  Comment AST extras are never executable CFG Statement nodes.
- JavaScript/TypeScript/ArkTS, Java, C#, PHP, Python, Kotlin, Cangjie, and Ruby
  lower try/catch/except/finally-style regions with path-isolated finally/ensure
  clones. Ruby covers method-body and nested begin/rescue/else/ensure. Normal,
  return, throw, break, continue, redo, and retry continuations cannot cross
  into one another; nested throws resume the enclosing handler path. Ruby
  lexical-loop and modeled block-resource `redo` restart the current body entry
  without reevaluating the loop condition or iterator call. Rescue-owned
  `retry` restarts the protected begin dispatch, after nested ensure/resource
  cleanup but before the same rescued begin's ensure. One try region is capped at
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
  exception identity, Ruby ordinary iterator/callback block bodies, computed
  goto, C# `goto
  case/default`, C++ cross-scope destruction on goto, and grammar-hidden labels
  remain explicit boundaries.
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
