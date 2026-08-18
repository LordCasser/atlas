# Changelog

All notable changes to Atlas will be documented in this file.

---

## [Unreleased]

### MCP Focus convergence

- Materialize every deferred inventory-only search candidate as an explicit
  Focus seed instead of grouping candidates that are not yet present in the
  `files` table into an unreachable same-directory closure.
- Keep a completed no-progress file window terminal. Scoped search therefore
  converges to a bounded gap instead of cycling through
  `tasks(status=ready)` and another retryable `resume_query` forever.

### Cross-language product-path parity

- Add one shared Focus-vs-full-Index baseline matrix for TypeScript,
  JavaScript, Python, Java, C, C++, ArkTS, C#, and PHP. Each fixture verifies
  that on-demand function materialization persists the same bindings, local
  dataflow, and CFG as full Index while the unit is cold beforehand.
- Keep feature-specific parity fixtures for Go, Rust, Ruby, Kotlin, Cangjie,
  and PHP, where the language boundary needs stronger assertions than the
  common baseline.

### Cross-language binding identity

- Promote `scope_aware_binding` for TypeScript, JavaScript, ArkTS, Java, C,
  C++, Go, Rust, Kotlin, Cangjie, PHP, and Ruby after direct extraction, SQLite Trace, and Focus-vs-full-Index
  fixtures proved that same-name locals retain distinct `BindingId` and scope
  ownership. Java uses legal sibling-block redeclarations because overlapping
  local redeclaration is rejected by the language. Go additionally covers
  same-block mixed short declarations and the function-body parameter
  exception. Rust pattern projection, Kotlin smart-cast, and compiler-grade
  variable-initialization proof remain explicit language-specific limitations.
- Align frontend lexical/dataflow slot confidence and limitation text with the
  authoritative language profiles. ArkTS remains a TypeScript-grammar boundary:
  ordinary nested blocks are verified, while ArkUI callback ownership and
  trailing-block internals remain best-effort.
- Remove the unused slot-derived capability-profile path. Static
  `LanguageCapabilityProfile.features` remains the single runtime gating truth;
  slot capabilities are implementation contracts checked against it, not a
  second user-visible profile.

### Go dataflow

- Model a type-switch alias as one distinct binding in every case/default
  clause's implicit block. The guard value flows conservatively to each alias,
  while the alias identifier in the guard is not misclassified as a read.
- Cover extraction, SQLite Trace, and Focus-vs-full-Index parity with the Go
  standard library's `context.stringify` shape. Shared lexical post-processing
  now assigns every function-local `BindingDef` to its innermost callable, so
  function-unit materialization cannot silently drop bindings.
- Resolve mixed short declarations to one canonical binding for source-earlier
  names in the same block, including parameters redeclared in a function body.
  Newly introduced names activate after the declaration, so nested initializers
  still read the outer binding; switch/select clauses retain sibling identities,
  and blank identifiers create no binding/dataflow facts.
- Cover the mixed-declaration boundary through direct extraction, SQLite Trace,
  and Focus-vs-full-Index parity. Case-type projection, function-literal
  ownership, select receive-clause flow, and parallel-assignment evaluation
  order remain conservative.

### Rust dataflow

- Model `match` guard `let` chains with source-ordered binding activation:
  each RHS resolves before its new pattern binding becomes visible, later
  guard operands and the arm body reuse that binding, and every guard value
  flows to its capture. SQLite Trace and Focus-vs-full-Index fixtures cover
  same-name shadowing and persisted activation boundaries.
- Bind bare, tuple/tuple-struct, struct shorthand/renamed, `ref`/`mut`, `@`,
  slice, and canonical or-pattern captures in an isolated `match_arm` scope.
  The match scrutinee flows conservatively to each capture, while guard and
  arm-body uses resolve the same binding identity.
- Cover nested-match ownership, constructor/type rejection, SQLite trace, and
  Focus-vs-full-Index parity. Structural projection, borrow/move modes,
  guard control dependencies, and syntactically ambiguous single-segment
  constants remain explicit precision boundaries.

### Storage

- Upgrade SQLite to Schema V4. `BindingDef.visible_from_byte` persists the
  source position where a lexical definition becomes eligible for lookup,
  allowing ordered shadowing and Rust guard-let chains to share one resolver.
  Atlas intentionally has no development-schema migration chain; remove the
  old `.atlas/atlas.db` and rebuild the index.

### Cangjie dataflow

- Model simple, nested-tuple, and enum-payload `for-in` captures as loop-scoped
  bindings while excluding enum constructor syntax. The iterable flows
  conservatively to every capture as aggregate provenance, guard/body uses
  share their identities, and outer same-name bindings are restored after the
  loop.
- Bind simple, tuple, enum-payload, and type-pattern captures in an isolated
  `matchCase` scope. The match selector flows conservatively to each capture,
  while guard and arm-body uses resolve the same binding identity.
- Cover extraction, SQLite trace, nested-match selector ownership,
  simple/tuple/enum-payload `for-in`, and Focus-vs-full-Index parity. Exact
  iterator element/structural projection, compiler validation of pattern
  irrefutability, guard control dependencies, guarded/composite exhaustiveness,
  other tuple/destructuring, and resource bindings remain explicit precision
  boundaries.
- Verify ordinary nested-block shadowing against the Cangjie language scope
  rules through direct extraction, SQLite Trace, and Focus-vs-full-Index parity,
  then publish `scope_aware_binding` for parameters and simple locals.

### Kotlin dataflow

- Capture fieldless tree-sitter-kotlin simple assignments through their
  `directly_assignable_expression` wrapper and preserve every concrete write
  origin for a typed local declared before a branch. A declaration without an
  initializer remains a lexical binding, not a source-free dataflow
  definition, so SQLite Trace reaches a real branch RHS instead of terminating
  at the variable name. Direct extraction, SQLite Trace, and
  Focus-vs-full-Index fixtures cover the boundary; compiler-grade
  variable-initialization proof and smart-cast type refinement remain
  conservative.
- Preserve distinct `BindingId` values for same-name Kotlin locals in nested
  control scopes. Extraction, SQLite Trace, and Focus-vs-full-Index fixtures
  prove that an inner read resolves the inner declaration while a later read
  resolves the outer declaration; smart-cast and compiler-grade
  variable-initialization proof remain conservative.
- Model `when (val V = E)` as initializer-to-subject assignment dataflow and
  resolve condition, guard, and body uses to the same scoped binding identity.
  Extraction, SQLite trace, and Focus-vs-full-Index parity fixtures cover the
  boundary; smart-cast, compiler-grade variable-initialization proof,
  type/range condition projection, and guard control dependencies remain
  conservative.
- Treat unresolved callsites as absent from resolved-callsite lookups instead
  of decoding nullable symbol IDs as query errors. Trace consumers therefore
  retain the same success envelope when a Kotlin initializer calls an external
  symbol.

### Ruby dataflow

- Bind local targets in flat, nested, and rest multiple assignment through the
  existing source-ordered method/module/class/block namespace. A block target
  reuses an earlier ancestor binding while newly introduced names remain local
  to that block.
- Pair explicit RHS lists with top-level targets by position. A single
  aggregate RHS, nested destructuring, and rest targets retain conservative
  group/slice flow; field and global targets use the existing `FieldStore` and
  `Assign` edges without adding persistent entities.
- Cover direct extraction, SQLite Trace, and Focus-vs-full-Index parity. Exact
  structural element projection, `to_ary` coercion, implicit `nil` fill,
  parallel evaluation order, and numbered parameters remain explicit bounds.

### Ruby control flow

- Persist dedicated `Redo` edges from lexical while/until/for and modeled
  resource blocks to the current body entry without reevaluating the loop
  condition or yielding call. Ruby postfix `if`/`unless` bodies now participate
  in CFG lowering with the correct condition-edge polarity.
- Lower postfix while/until with language-accurate entry order: plain modifier
  bodies are pre-test, while `begin ... end while/until` bodies execute once
  before the first condition. `next`, `redo`, and `break` retain their distinct
  condition/body/join targets, and Focus CFG edges match full Index facts.
- Lower Ruby `case ... in` as non-fall-through sibling paths. Refutable cases
  without `else` retain the implicit `NoMatchingPatternError` as a `Throw`
  path, while an unguarded capture is syntax-irrefutable. Ruby, Python, Rust,
  Kotlin, and Cangjie match-like constructs no longer consume an enclosing
  loop's `break` as though they were C-style switches.
- Bind Ruby `case ... in` bare、key-only、`=>` and array/hash-rest captures in
  the enclosing local namespace and conservatively flow the whole match subject
  to each capture. Guards and bodies resolve the same binding identity；pins
  remain reads. Structural projection and post-match path-definedness stay
  explicit precision boundaries.
- Persist rescue-owned `Retry` edges back to the protected begin dispatch.
  Nested ensure/resource cleanup runs before retry, while the ensure belonging
  to the retried begin is bypassed until that begin eventually completes.
  Abrupt cleanup still overrides the incoming continuation.
- Add adversarial extraction and extraction→SQLite persistence coverage without
  changing the database schema or MCP/TUI response contracts. Ordinary
  iterator/callback blocks remain an explicit gap.
- Resolve Ruby block assignments in source order: writes reuse an existing
  ancestor binding, while new block locals and shadowing block parameters keep
  their own identity. A later outer assignment does not retroactively capture
  an earlier block local; numbered parameters remain an explicit boundary.

### PHP dataflow and control flow

- Extract `[]` and `list()` nested, keyed, and by-reference destructuring
  targets into the callable namespace for assignment and `foreach`. Key
  expressions remain reads. Assignment conservatively flows the whole RHS to
  every supported target at confidence 0.75; `foreach` flows the collection to
  direct and nested key/value targets at 0.65. SQLite Trace and Focus-vs-full-
  Index fixtures cover persisted identity and flow. Exact key/index projection,
  missing-key/null behavior, reference-alias semantics, and dynamic or
  non-variable targets remain explicit boundaries.

- Model extracted PHP variables in file/function namespaces instead of
  structural `if`/loop/block scopes. Foreach collection expressions are no
  longer declarations; direct key/value bindings retain one identity inside
  and after the loop, and `$` normalization is shared by BindingDef,
  BindingUse, and DataNode facts. Extraction, SQLite Trace, and Focus-vs-full-
  Index fixtures cover the boundary. Assignment-created locals and explicit
  anonymous-function captures now use scope-chain identity; unresolved names
  stop at function/method boundaries. Anonymous-function dataflow remains in
  the enclosing named-function materialization unit. Global aliases, variable
  variables, compound/update assignment edges, and arrow-function ownership
  remain conservative.
- Resolve direct same-function PHP `goto` to standalone label `Join` targets
  through deterministic persisted `Goto` edges.
- Execute every intervening path-isolated `finally` clone from inner to outer;
  abrupt finally completion overrides the pending goto continuation.
- Match PHP's region rules: ordinary-block entry and loop/switch exit are
  allowed, loop/switch entry and either-direction finally-clause crossing are
  rejected. Add adversarial extraction coverage and an inline
  extraction→SQLite-persistence fixture without changing the schema or public
  MCP contract.

### C# dataflow and control flow

- Extract direct declaration/recursive/var pattern bindings from `is`, switch
  statement, and switch-expression syntax. Switch sections and expression arms
  own distinct lexical scopes, so same-name captures do not cross sibling arms;
  the matched subject flows conservatively to every direct capture.
- Recursively bind parenthesized nested designations such as
  `var (first, (second, third))` in their owning switch arm. Direct captures
  retain whole-subject `Assign` flow at confidence 0.80; nested designation,
  property, and list captures use aggregate subject flow at 0.72 because Atlas
  does not claim the extracted component equals the whole matched value.
- Cover direct extraction, SQLite Trace, and Focus-vs-full-Index parity,
  including persisted confidence and a cold unit that leaves its peer method
  unmaterialized. Exact property/index/positional projection, compiler
  definite-assignment, and guard control dependency remain explicit precision
  boundaries.

- Route direct `goto` exits through every intervening `using` `BlockExit` and
  path-isolated `finally` clone from inner to outer before emitting the final
  `Goto` edge. Same-region jumps do not execute cleanup early.
- Reject jumps into nested lexical/cleanup regions and jumps out of a `finally`
  clause without guessing a target edge. `goto case/default` remains deferred.
- Add extraction-level adversarial coverage and an inline
  extraction→SQLite-persistence fixture for nested cleanup-crossing goto. No
  SQLite schema or public MCP contract changed.

## [1.6.1] - 2026-07-27

All changes **after tag `v1.6.0`** belong to 1.6.1. This is an indexing
performance release: no MCP tool names, schemas, trace envelopes, SQLite schema
version, or extraction semantics changed. Index contents are byte-for-byte
identical to 1.6.0 — verified by comparing `symbol_edges`, `data_nodes`, and
`dataflow_edges` row counts plus the full resolution strategy distribution
(`s1..s6`, `miss`, `s6_exact`/`s6_fuzzy_prox`/`s6_fuzzy_global`) before and
after every change.

### Indexing performance

- **Bulk-load index staging is now per phase, not per pipeline.** Edge building
  ran with `idx_symbols_name`, `idx_callsites_reference`, `idx_data_nodes_file`,
  and `idx_dataflow_edges_target` dropped, so callback/function-pointer
  resolution degraded to full table scans against million-row tables. Those four
  indexes moved into the resolution stage. Edge building on a full-analysis run
  dropped from **158.8s to 11.6s**.
- **Function-pointer resolution pushes its predicate into SQL.** The C
  function-pointer path loaded every data node in a file and filtered in Rust —
  27.7M rows scanned across 6,195 calls. Added
  `Store::find_data_node_at_range`, which matches file, kind, and byte range in
  one indexed lookup. The parallel edge loop went from **11.4s to 1.1s**.
- **Summary building no longer rescans or recompiles.** The dataflow BFS used a
  linear `nodes.iter().find(...)` per discovered edge (498M row comparisons on
  redis) and loaded edges through an `IN (?1..?N)` clause that recompiles SQL for
  every distinct node count. Replaced with an up-front `HashMap` and the static
  `find_dataflow_edges_by_function` join. `summary_build` **5.36s → 4.60s**.
- **Strategy 6 no longer takes a mutex per reference.** Import-scope lookup
  consulted a shared `Mutex<HashMap<FileId, HashSet<FileId>>>` for every
  reference; 34.1s of a 94.4s strategy-6 phase was pure lock traffic. The scope
  set is now computed once per file by the caller. `s6_s` **94.4s → 48.2s**.
- **Proximity fuzzy search filters by directory first.** The directory-proximity
  predicate rejected 97.7% of candidates but ran *after* length and trigram
  filtering, so 851M rows were examined to keep 528K. `GlobalSymbolIndex` now
  carries a directory→symbol index. Proximity fuzzy search **47.3s → 4.90s**.
- **SQLite reads use a connection pool.** All 145 read call sites shared one
  `Mutex<Connection>`, serializing every rayon worker in the parallel resolution
  phase. `StoreReader` now holds `read_pool: Vec<Mutex<Connection>>` sized to
  `available_parallelism()` clamped to 2..8, selected thread-affinely with
  `try_lock` probing. The shared 64 MiB read page-cache budget is divided across
  the pool (floor 16 MiB per connection) so peak memory does not scale with core
  count.
- Summary indexes are created whenever bulk-load dropped them, not only on
  dataflow runs. This removes the `missing schema indexes missing_count=11`
  warning that structural and manifest runs emitted during finalization.

Measured end to end (release build, cold `.atlas`):

| Workload | 1.6.0 | 1.6.1 |
| --- | --- | --- |
| TypeScript monorepo (2,644 files), structural | 26.1s | **18.2s** |
| TypeScript monorepo (2,644 files), full | 47.9s | **40.1s** |
| redis (846 files), full | 21.6s | **17.6s** |

### Progress reporting

- Indexing throughput is now a 5-second sliding window instead of a
  phase-cumulative average. The old formula divided total completed items by
  total phase elapsed time, so a stalled counter produced a smoothly decaying
  rate that looked like slow progress instead of a stall.
- Edge building reports its serial tail stages (`detecting callbacks`,
  `detecting decorators`, `writing edges`), which previously ran with the
  progress counter pinned at 100%.

### Documentation

- `docs/performance.md` records the seven optimizations (P7–P13), the rejected
  experiments, and methodology sections §10–§17 covering per-phase index
  staging, sliding-window progress, predicate pushdown, `IN (?1..?N)` as
  disguised dynamic SQL, cache-key placement, filter ordering by selectivity,
  and connection-per-thread reasoning.
- `README.md` documents the Grok MCP client configuration.

### Version

- Workspace package version and skill metadata: **1.6.1**.

## [1.6.0] - 2026-07-22

All changes after tag `v1.5.5` form the CFG v3 milestone. The public MCP V1
tool names and trace envelope remain stable; persisted index semantics move to
SQLite Schema V3 and require rebuilding an older `.atlas/atlas.db`.

### CFG v3 and persistence

- Persist dedicated `CaseBranch`, `Break`, `Continue`, `Goto`, `Defer`, and
  `Exception` edges, deterministic lowering-instance node identities, managed
  scope ownership, call context, and branch-context facts.
- Cover limited function/method CFG for all 14 default languages, including
  switch/match/select sibling paths, language-correct fall-through, lexical
  break/continue labels, and direct same-function goto for C/C++/Go.
- Add cleanup-safe C# direct goto/label resolution for functions without
  `finally`/`using` ownership. `goto case/default` and cleanup-crossing goto
  remain conservative terminals. Real Shadowsocks `Listener.ReceiveCallback`
  verifies extraction through SQLite persistence.
- Ignore comment AST extras as executable statements, including comments
  between a label and its first executable body node.

### Exceptional and managed control flow

- Lower try/catch/finally-style control flow with path-isolated normal and
  abrupt continuations for JavaScript/TypeScript/ArkTS, Java, C#, PHP, Python,
  Kotlin, Cangjie, and Ruby; C++ exposes explicit exception paths.
- Route Java try-with-resources, C# using, Python with, Kotlin use, and Ruby
  resource blocks through owner-matched `BlockExit` nodes with deterministic
  LIFO cleanup effects. Cleanup exceptions retain conservative ordered
  continuations into enclosing handlers/finally regions.
- Cap clone expansion at 64 per isolated region and fall back atomically rather
  than persisting a partial graph.

### Language precision

- Align Python lexical facts with Python namespaces: ordinary statement blocks
  no longer create shadow scopes, repeated writes share one binding identity,
  comprehensions remain isolated, and structural match capture/`as`/star
  bindings receive conservative subject-flow edges through guard/body uses.
- Go switch/select preserves blocking semantics; bounded path-sensitive defer
  stacks execute registered calls in LIFO order on normal exits.
- Rust `?` retains success plus residual-return paths; `let-else` separates the
  success path from abrupt alternatives; standalone unqualified
  `panic!`/`unreachable!`/`todo!`/`unimplemented!` terminate the local path.
- Python syntax-irrefutable match patterns and direct Rust/Cangjie wildcards
  suppress impossible synthetic no-match paths. PHP, Ruby, Kotlin, and Cangjie
  receive expanded grammar-specific branch, exception, and resource coverage.
- Capability profiles and architecture documentation state the remaining
  macro, unwind, pattern, callback, cleanup-identity, and computed-jump limits
  explicitly instead of implying compiler-grade CFG precision.

### Analysis, Focus, and verification

- Lifecycle, branch diff, semantic effect composition, Focus materialization,
  Index mode, SQLite readers/writers, CLI status, and MCP analysis consume the
  same structured CFG facts.
- Add cross-language Golden, extraction, persistence, Focus, MCP, and real
  project regression fixtures for the schema v3 semantics.

### TUI, MCP, and mixed-index alignment

- Determine FullCache authority per QueryNeed from finalized whole-repository
  Index metadata plus fresh complete coverage for every file. Partial Focus
  enrichment preserves lower-layer authority without promoting CallGraph or
  dataflow queries; CallGraph/dataflow also require current canonical resolution
  fingerprints for reference-bearing files after source-changing Focus rebuilds;
  replacing a target also revokes fingerprints of importers whose resolutions
  were invalidated.
- Make manifest file-dependency queries pure reads and align structural mode on
  the CallGraph contract. `tasks` now preserves failed Focus/extraction work as
  a failed terminal state instead of reporting ready.
- Keep MCP responses as one complete JSON document; handlers enforce schema
  maxima and expose explicit returned/truncation metadata instead of slicing
  serialized JSON. Project files default to 500/cap at 1000; graph, trace,
  dossier, context, task, and overlay collections have hard resource bounds.
- Align TUI forms with MCP defaults and request-scoped `include_roots`. Partial
  catalogs open symbol context through the shared focus-aware handler; shared
  tool completion invalidates the separate native graph snapshot and refreshes
  Store-derived status.
- Replace the seven-position-argument file inventory write API with the typed
  `DiscoveredFile` Tier-0 record; content fingerprints remain a later Tier-0.5
  concern rather than being mixed into discovery metadata.

### Version

- Workspace package version and skill metadata: **1.6.0**.

## [1.5.5] - 2026-07-14

All changes **after tag `v1.5.4`** belong to 1.5.5 (commits since the tag plus
in-tree work). 1.5.4 remains sealed at that tag.

### Index / Focus query semantics

- Preserve structured signatures and declaration source across extraction and
  MCP detail/search; align ArkTS/TypeScript golden fixtures.
- Expose persisted import/export facts through `explore` / dossier file-facts
  paths; derive catalog tier from finalized index state.
- Bound cold discovery; Focus query preparation converges without blocking tool
  calls on full-repo work.
- Store helpers for file inventory / extraction state / edges support the aligned
  Focus vs FullCache read path.

### MCP complete results (no provisional payloads)

- Every Focus-backed query uses one **18s interactive window**. If required
  facts land in time, MCP **replays the snapshot** and returns a complete result.
- On timeout: only `status=in_progress`, `query_id`, `pending.*`, and
  `analysis.retry_after_ms` — **never** provisional result arrays, paths,
  callers, or trace payloads. Resume via `resume_query`.
- On materialization failure: result-free `status=failed` ticket; re-run the
  original tool to retry (failed work is not published as a limited result).
- Shared `QueryNeed` (`manifest` / `structural` / `call_graph` / `dataflow`)
  between MCP contracts and Focus control plane; tool_contract / resume / status
  / trace handlers updated accordingly.
- Docs: architecture non-terminal envelope, `docs/trace-contract.md`,
  `crates/atlas-mcp/README.md`, skill tool-guide.

### Source encoding (GBK / Western 8-bit)

- Unified source reader `workspace::read_source` / `decode_source` (chardetng +
  encoding_rs): non-UTF-8 sources (notably **GBK**/GB18030 Chinese, **windows-1252**
  for ISO-8859-1-class Western text) decode to UTF-8 **in memory only**; disk
  files are never rewritten.
- **File identity hash** (`files.content_hash`, dirty, fingerprint, stale) is
  `blake3(raw bytes)`. Partial content digests use `blake3(decoded UTF-8)`.
- Public `Engine::extract_file_with_mode` accepts `SourceText`, so direct engine
  consumers preserve the raw-byte file hash while parsing decoded UTF-8 text.
- Index, Focus structural/dataflow, bootstrap, source extractors, context,
  dossier, and trace snippets all use the unified entry.
- Contracts: `docs/architecture.md` §3.2 and `docs/requirements.md`; required tests
  (`docs/testing.md` §2.1.1):
  `cargo test -p workspace --lib source_text`,
  `cargo test -p extraction --test source_encoding_extract`,
  `cargo test -p filesync --test source_encoding_index`.
- Atlas Skill documents that legacy-source ranges address the decoded UTF-8 view,
  not raw-file edit coordinates.

### Version

- Workspace package version and skill metadata: **1.5.5**.

## [1.5.4] - 2026-07-13

### ArkTS analysis boundary

- ArkTS declarative components now preserve `struct` members, `build()` scope,
  UI call ownership, and member-call receivers through byte-stable parser
  normalization. ArkUI trailing blocks may still report `partial`, but their
  usable structural facts no longer collapse to global calls. Query-time trace
  adds `StateFlow` from ArkTS `AppStorage.set/setOrCreate` values to matching
  `@StorageProp`/`@StorageLink` field reads and UI call arguments. Matching is
  syntactic but preserves literal-vs-expression identity and requires an exact
  `this.<field>` read. Cold Focus traces add a state-channel closure, materialize
  cross-directory writer dataflow, and converge through `resume_query`.
  Reverse links, default initialization, timing, and process boundaries remain
  explicitly unmodeled.
- ArkTS parameterized component decorators plus trailing ArkUI chains can make
  the TypeScript grammar emit a fallback `class` expression instead of
  `class_declaration`. ArkTS-owned definition/manifest/scope queries now cover
  both forms; struct scope range recovery balances braces while excluding
  parsed strings, templates, regexes, and comments. Focus rejects persisted
  type ranges with inverted line intervals, and `SourceExtractor` rejects
  truncated AST definitions in favor of the complete stored range.
- ArkTS declaration extraction now rejects nested ArkUI recovery methods by
  ownership instead of a narrow object shape. A byte-stable declaration-only
  recovery tree restores post-build `@Styles` methods and top-level `@Extend`
  functions without changing the primary references/dataflow/CFG tree.
  TypeScript-compatible abstract classes, interface properties/methods, enum
  members, async flags, and decorator references now populate existing IR;
  member source extraction includes field decorators, types, and initializers.
- Declaration recovery rewrites complete fake ArkUI method headers to valid,
  byte-stable control headers. Deep `Navigation`/`List` component trees no
  longer erase their owning struct, `build()` method, fields, or following
  `@Builder` functions.
- ArkTS keeps callable signatures limited to interface shape: decorators remain
  `Decoration` references, field types remain in complete member source ranges,
  and `async` remains in `SymbolDef.async_`. `ScopedSearchService` owns exact
  `@Decorator` search, including scope-wide structural coverage, lazy
  refinement, kind/language filters, limits, and total counts. Mixed
  manifest/structural scopes no longer report incomplete decorator totals as
  complete. Decorator source recovery skips parser-recognized strings,
  templates, regexes, and comments while balancing parameter lists.
- Decorated declaration ranges now cover stacked decorators across languages;
  Python decoration references preserve the full decorator range while keeping
  a bare resolution name. Full and file-scoped graph builds both materialize
  callback and Python decorator registration edges from that ownership fact.
- Builtin resolution is terminal before project-wide matching. ArkTS `$r`
  remains an external resource API instead of resolving to an unrelated
  JavaScript symbol; ArkTS decoration references likewise remain framework
  externals instead of generating Python-style callback edges. Overlapping
  tree-sitter query captures are deduplicated by capture/node identity before
  normalization.
- ArkTS named function/method CFG is enabled through the shared TypeScript
  walker at confidence 0.55. Branch/loop fixtures assert concrete nodes and
  edges. ArkUI trailing blocks remain single statements and nested arrow
  callbacks do not receive independent CFGs; official language restrictions
  are not treated as validated invariants because Atlas does not run the ArkTS
  compiler.

### TypeScript module facts

- TypeScript/JavaScript/ArkTS imports now emit one `ImportDef` per binding or
  side effect instead of redundant module/name/alias rows. Default imports are
  captured explicitly, aliases preserve source and local names, wildcard facts
  are marked correctly, and every import range covers its complete statement.
  Query-only predicate captures no longer leak normalization diagnostics.
- Default imports and exports preserve their source-to-visible-name mapping.
  Re-export resolution starts from actual module files and follows named
  aliases, wildcard barrels, and default chains deterministically. Explicit
  bindings remain unresolved when the target module does not export them,
  including private same-name declarations; wildcard re-exports do not leak
  `default`. `ReferenceKind::Instantiation` now produces `Instantiates` edges.

### Release hardening

- Upgrade note: extraction and module-resolution facts changed without a schema
  version change. Existing projects must remove `.atlas/atlas.db` and run
  `atlas index --project <path> --analysis <mode>` to materialize 1.5.4 facts;
  `doctor` cannot distinguish pre-1.5.4 facts with unchanged source hashes.
- Workspace packages and release metadata advance to 1.5.4.

---

## [1.5.3] - 2026-07-10

### Focus materialize (query-time stack)

- Product paths remain **Index** (pre-materialize) and **Focus** (query-time).
  On-demand structural/dataflow is Focus-internal materialize, not a third
  product line; package `focus_materialize` (mechanism types still `Lazy*`).
- Single stack: `FocusMaterialize::open` wires structural + dataflow +
  structural rebuilder; `FocusRuntime` / MCP `ActiveProject` / `Engine` /
  `AnalysisRuntime` share one Arc stack (`from_materialize` / `same_stack_as`).
- Removed obsolete MCP `init_focus` no-op; no silent second materialize on prepare.
- Unit dataflow write: invalid `data_node.binding_id` is cleared (SET NULL) so
  Focus ensure no longer drops most unit facts vs Index full (FK guard).
- Unit `FactCoverage::CALL_EDGES` gated on fresh structural layer + real
  callsites (same helper for ensure + prebuilt paths); capability regression
  fixtures now require callsite-free units to keep the bit unset.
- N5 e2e: neighborhood structural/dataflow slices and `FocusRuntime::prepare`
  parity vs Index (`focus_materialize_e2e`, `docs/testing.md` §2.6.2).
- Shared `apply_post_extract_hooks` for Index and Focus structural (Linux export
  / initcall, etc.).
- Focus writes **reject** when another process holds CLI `FileLock`
  (`cli_index_lock_held`); no wait/queue.
- WindowBudget foreground default `max_iterations=0` (prepare overrides via
  `iterations_for`); CallGraph strategy `depth!=1` hard-errors.
- MCP `calls` incoming/outgoing fixed 1-hop + `signature` field; multi-hop via
  `direction=both`/`depth` or callgraph path.
- Cross-function trace docs: Focus uses Phase 2 runtime BFS as primary path.
- Workspace `[workspace.package]` version/edition unified to 1.5.3.
- `cargo fmt --all` applied across the workspace (40 files reformatted, no
  behavior change).
- DEBT-8 foundation: drop dead `AnalysisNeeds::Cfg`/`CfgAndDataflow`; add
  `handler_purity` allowlist ratchet + full V1 `contract_for` coverage tests.
- DEBT-8 analysis migration: `AnalysisRuntime` is the real dispatcher for
  `lifecycle` / `branch_diff` / impact-semantic (capability gate, dataflow I/O,
  effect composition, engine call). Handlers parse args + render envelopes only.
  Impact handlers now pass only graph-selected symbol IDs to
  `run_semantic_impact`; persisted C/C++ alloc/free/cleanup rules are merged
  into the language's default effect-composition contract, and semantic field
  aggregation is deterministic.
  Purity allowlist shrinks to 3 (`mod.rs`, `annotations.rs`, `active_project.rs`);
  unused allowlist entries fail; dual ratchet (engine-name + orchestration
  patterns including `find_cfg_*` / compose / rules load).
- DEBT-8 ratchet completion: god-router no longer takes `focus_runtime.lock()`
  directly (new `QueryRuntime` delegates `enqueue_file_focus_warm` /
  `focus_materialize_has_structural_rebuilder` / `focus_materialize_same_stack_as`;
  the `focus_runtime` field is now private). Annotation test seeds route through
  `overlay_runtime` instead of `store.upsert_fp_annotation(`. Purity allowlist
  shrinks to 1 (`active_project.rs` project-open factory - construction-time
  `FocusMaterialize::open`, a documented legitimate exception); residual ceiling
  is now `assert!(allowlist.len() <= 1)`.
- BUG-6 fresh-call graph refresh: `JobTracker::record_built_files` now retains
  both stable per-job history for `resume_query` and a deduplicated one-shot
  project refresh feed. `maybe_refresh_graph` drains that feed through
  `FocusRuntime` / `QueryRuntime` before `take_incremental_batch`, independent
  of `replay_focus_result`. Fresh graph requests therefore observe completed
  closure and file-warming writes without cross-request closure IDs or an
  engine-to-MCP callback registry; resume remains idempotent. Failed
  incremental refresh batches are requeued without inflating the cumulative
  lazy-write count, so the one-shot feed is not lost on transient errors.
- Facade narrowing (breaking): remove zero-call planner, pipeline phase,
  parser-pool, summary-store, resolution-session, and config-helper re-exports
  from `atlas-engine`. Stable entry points now re-export the concrete argument
  and return types needed to name their public signatures. Delete the unused
  `JobContext` and the dead ClosurePlanner workset/sibling/regex-bootstrap
  branches that public reachability had hidden from dead-code analysis. A
  source ratchet prevents pipeline mechanisms from returning to the facade.
- God-file reduction (structural only): extract inline test modules and
  isolate handlers by domain. `mod.rs` moves 3,372 lines of inline
  `tools::tests` to `mod_tests.rs` and 1,544 lines of `graph::tests` to
  `graph_tests.rs` (module identity preserved). `graph.rs` splits its four
  handlers into `graph/calls.rs` (706), `graph/path.rs` (643),
  `graph/explore.rs` (393), and `graph/impact.rs` (237). `mod.rs` further
  extracts 418 lines of tool schemas to `tool_schemas.rs` and 7 entry
  handlers to their domain modules (`handle_calls` -> `graph/calls.rs`,
  `handle_project` -> `open_project.rs`, `handle_symbol` ->
  `search.rs`, `handle_fp_dispatches` -> `annotations.rs`,
  `handle_domain_rules` -> `domain_rules.rs`, `handle_tasks` ->
  `atlas_jobs.rs`, `handle_file_dependencies` -> `file_deps.rs`). Final:
  `mod.rs` 5,973 -> 1,322 (pure core orchestration), `graph.rs` 3,763 ->
  330 (pure shared helpers). Dependency direction single (child -> parent,
  no reverse). Handler decomposition complete.
- Fix CallGraph stub test for depth=1 hard error + explicit WindowBudget.
- Shared exclusive-lock reject diagnostic on `Store` (filesync + dataflow loader DRY).
- Lock Task 3 calls 1-hop/depth-warning/signature tests; Focus Phase2 ArgToParam
  without summary (Task 6).
- Docs: architecture/testing/roadmap/requirements record DEBT-8 current facts
  (dispatcher ownership, purity dual guard, residual allowlist, §2.11 test matrix);
  change history lives in this file.
- Symbol resolve UX: plain-string lookup falls back to simple `name` when exact
  `qualified_name` misses (e.g. `GetDev` → `CertUtils::GetDev`); multi short-name
  hits return `Ambiguous` with full qnames + `symbol_ref` for disambiguation.
  C++/PHP qualified calls capture the last name segment so Calls edges resolve
  (re-index required for existing projects). PHP normalizer mirrors C++ for
  full `text`/`receiver`; nested C++ `A::B::C` keeps outermost span; extraction
  + resolution tests lock `CertUtils::GetDev` / `\Foo\bar` and Calls edges.
### Release hardening

- `atlas doctor` now reports Atlas version, Schema V2 state, canonical index
  mode, compiled features, and per-language capability profiles.
- Fresh databases are stamped with `CURRENT_SCHEMA_VERSION`; non-empty
  unversioned development databases are rejected with rebuild guidance instead
  of being silently migrated.
- MCP trace JSON contract coverage now exercises the full
  `ToolRouter::call_tool()` path and locks `query_id`, `analysis`, V1 trace
  envelope fields, and retired-field exclusions.
- `docs/performance.md` includes a release-mode Atlas self-index smoke baseline
  for the 1.5.x line.
- README release documentation now records the source/binary distribution
  decision, release build command, feature choice, and platform asset matrix.

---

## [1.5.2] — 2026-06-23

### Focus Runtime — Cold Start & Fixed-Point Precision

The closure engine (query-time focus) has been substantially reworked to scope
cold-start expansion around individual symbols rather than entire files.
Previously, querying a symbol pulled the whole file into the structural closure;
now only that symbol and its direct call/type dependents are expanded.

- **Symbol-scoped cold start**: `FocusSeed::Symbol` carries an optional `file_id`.
  When the seed symbol's file is already known, the engine skips the candidate
  provider and expands only that symbol's direct dependents.  Stack traces,
  lifecycle, and branch-diff queries are now function-local (`QueryIntent::
  SemanticFunction`) — they need only the seed file, not call-graph expansion.
- **Import/Include as resolution-only boundary**: import dependency files are
  materialized for *resolution symbols only* (so call targets can be resolved),
  but are never expanded into the structural closure.  The closure no longer
  fan-outs across all includes.
- **Fixed-point TypeGraph**: TypeGraph expansion now participates in every
  planning round of the fixed-point loop alongside CallGraph — previously it was
  a single-prefetch Phase 2 that never iterated.
- **Full-closure resolution**: resolution re-evaluates every file in the bounded
  closure on each iteration (was incremental "new files only"), because new
  dependency symbols can change resolutions in previously-visited files.
- **Closure completeness**: `ClosureComplete` (high confidence) now requires a
  non-empty structural closure AND zero gaps.  Gaps are recorded via
  `record_extraction_outcome()` — extraction failures and budget exhaustions are
  now structured terminal gaps, not silent omissions.
- **JobTracker terminal state**: failed focus jobs are now terminal (entries
  become diagnostic gaps) instead of remaining permanently "refining."  Failed
  background work produces `background_refinement_failed` gap records; queries
  converge rather than hanging.

### Query-Time Lifecycle Effects

Domain-rule changes no longer require rebuilding the structural index.  The
lifecycle engine accepts a pre-composed `EffectComposition` at query time,
applied to an in-memory CFG copy; persisted CFG nodes remain raw.

- **`analyze_with_composition()`**: caller provides a composed `EffectComposition`
  with resource-op annotations (producer/consumer/leak/release).  The lifecycle
  pipeline applies them to a clean CFG copy without mutating persisted rows.
- **Local resource variable tracking**: `place_matches()` handles `Local` and
  `Indeterminate` place references in addition to struct field paths — the
  lifecycle engine now tracks resources assigned to local variables (`void *p =
  kmalloc(...)`) alongside struct members.
- **Linux kernel alloc/free functions**: `kmalloc`, `kzalloc`, `kcalloc`,
  `kmalloc_array`, `vmalloc`, `vzalloc`, `kfree`, `kvfree`, `vfree` added to
  the builtin C alloc/free lists.  `ownership_rules.rs` now queries these lists
  dynamically rather than hardcoding individual function names.
- **`KnownGap::ExtractionFailed`**: new gap variant for structured extraction
  failure reporting — consumed by both MCP and TUI layers.

### Extraction — Type Symbol Ranges & Stale Cache Self-Healing

- **Enum scope capture**: tree-sitter queries for both C and C++ now capture
  `enum_specifier` as a scope node.  `ScopeKind::Enum` is mapped in both `c.rs`
  and `cpp.rs` language drivers.
- **Full type symbol ranges**: `Class`, `Struct`, `Interface`, `Trait`, and
  `Enum` symbols now have their range expanded to the complete defining scope
  body (was only `Function`/`Method`).  This includes multiline definitions with
  the closing delimiter.
- **Stale type-range detection generalized**: the lazy structural cache validator
  (`has_complete_type_ranges`) now applies to all supported brace-based languages
  (was C/C++ only).  A Rust struct extracted with a one-line range will be
  detected as stale and rebuilt even when the content hash matches.

### MCP — Precise Intent Boundaries & Terminal Gap Handling

- **`SemanticFunction` intent**: `branch_diff` and `lifecycle` handlers now use
  `QueryIntent::SemanticFunction` — the engine extracts only the target
  function's file (structural, dataflow, CFG) without call-graph fan-out.
- **Query-time lifecycle composition**: the MCP lifecycle handler loads dataflow
  nodes and edges, builds `CfgGraph` at request time, calls `compose_effects()`
  with `ResourceOpConfig`, and passes the composition to `analyze_with_composition()`.
  Analysis basis is enriched with `"dataflow"` and `"effects"` entries.
- **Post-focus symbol re-fetch**: the explore handler re-fetches the symbol from
  the store *after* focus preparation.  The pre-focus snapshot may carry a
  stale single-line type range (declaration only); the post-focus copy has the
  full body range, which the dossier builder depends on.
- **`materialized_files()` for complete refresh**: replaces `built_files`
  (foreground-only) with `materialized_files()` (foreground + background
  from `JobTracker`).  Used by the lazy-refresh queue and `resume_query` replay
  — background-refined facts are now visible in the graph snapshot on resume.
- **Background refinement failures as terminal gaps**: `apply_focus_result_to_lr()`
  collects `JobTracker.failures_for()` and injects them as
  `background_refinement_failed` gap records.  Regression tests assert that
  failed background work produces a terminal (no-retry) response.

### TUI — Async Graph Loading & Non-Blocking UI

The TUI no longer builds the in-memory graph snapshot on the UI thread.
Native symbol search is available immediately via store-backed SQLite ranking.

- **`LoadGraph` background job**: `GraphEngine::from_store()` runs on the worker
  thread.  `GraphSession` is a passive owner — workers push completed snapshots
  via `install_graph()`; `needs_refresh()` reports staleness so callers submit a
  fresh `LoadGraph` rather than rebuilding inline.
- **Store-only search fallback**: `run_search()` uses `GraphEngine::empty()` when
  no graph snapshot is available, ranking results from SQLite facts with neutral
  degree scores.  Search, trace, and call jobs are accepted before the graph is
  ready.
- **Async detail view**: `show_symbol_detail()` submits a `LoadGraph` job and
  stashes `pending_detail_symbol` when the graph is absent or stale.  On
  `JobResult::GraphLoaded`, the snapshot is installed and the detail view renders.
- **Non-blocking job replacement**: the 100ms `sleep` in `JobManager::submit()`
  that blocked the UI thread during worker replacement has been removed.
  Cancellation is cooperative via cancel token; the old `JoinHandle` is detached.
- **`GraphEngine::empty()`**: new constructor for store-only operations whose
  graph signal is optional (symbol search during TUI startup, tests).

### Infrastructure

- **Crate versions**: `atlas-cli`, `atlas-engine`, `atlas-mcp` bumped to 1.5.2.
- **47 files changed**: +1,959 / −679 across the release cycle.

---

## [1.5.1] — 2026-06-21

### TUI 2.0 — Command Palette + Shared MCP Tool Pipeline

The TUI has been rebuilt around a command-palette architecture.  Instead of
hard-coded `i`/`v` keys with ad-hoc result strings, the TUI now shares the exact
same `atlas_mcp::tools::ToolRouter` that the MCP transport uses — same handlers,
same analysis envelope, same retry/gap semantics.

- **Command palette** (`:` key): typed parameter forms for all 15 analysis tools
  (`symbol`, `calls`, `explore`, `impact`, `path`, `trace`, `file_dependencies`,
  `lifecycle`, `branch_diff`, `domain_rules`, `fp_dispatches`, `tasks`,
  `resume_query`).  Fields are validated before submission; variant-dependent
  forms (e.g. `trace kind`) show only the relevant parameters.  No JSON required.
- **Human-oriented result projection**: tool output is rendered as structured
  sections — code facts, source, paths, dependencies, rules, diagnostics.
  Capability, confidence, coverage, and truncation metadata live in an adaptive
  HUD.  Unknown future fields are preserved in the facts view rather than
  silently dropped.
- **Raw JSON toggle** (`r`): the untouched handler response is always one key away.
- **Removed**: `AnalysisHud`, `ToolKind`, `InteractionMode` — analysis state is
  now supplied exclusively by handler responses, never inferred from index mode.
- `atlas-mcp` is now a regular library dependency of `atlas-cli`; the `mcp` Cargo
  feature only controls the stdio transport subcommand.

### MCP Response Envelope V2 — Terminality & Structured Gaps

The MCP response contract has been simplified around two public signals:
`analysis.retry_after_ms` (work in progress) and `gaps` (permanent limitations).

- **`GapRecord`** replaces `KnownGap`: each gap carries `scope`, `reason`, and
  `detail` — consumed by both the MCP transport and the TUI projection layer.
- **Removed public fields**: `storage_mode`, `capability_mask`, `triggered_lazy`,
  `precision`, `work`, `lazy_diagnostics`, `analysis_contract`.  Agent consumers
  no longer navigate six overlapping metadata surfaces.
- **Terminal gap semantics**: search and trace tools now return structured
  `closure_boundary` and `capability` gaps instead of spurious `retry_after_ms`
  when the constraint is permanent.
- **Symbol `view="detail"`** is a pure Store-fact query — no graph init, no lazy
  trigger.  Only `includeCode=true` with missing source begins lazy extraction.
- **Resume**: conditional graph refresh only for `SemanticGraphQuery` contract
  replays; background focus warming removed from search hot path.

### MCP Async Execution

- **`TaskManager`**: true async execution with a sync wait window.  Requests
  return non-terminal responses with `query_id` while background work continues.
- **Progress tokens**: forwarded without blocking the dispatch loop; the previous
  progress-token hang on sync requests is fixed.
- **`tasks` tool**: snapshot-based job tracking with `query` status field.
- **Background sync subsystem removed**: probe, spawn, and overlay mutation gates
  deleted.  File-change sync is CLI-only (`atlas sync`); MCP query paths do not
  probe or synchronize the working tree.

### Focus Runtime Hardening

The query-time control plane received targeted improvements without changing the
public MCP contract:

- **Scoped resolver rewrite**: closure resolution no longer builds a
  `GlobalSymbolIndex`.  Uses local `find_symbols_by_name` with proximity ranking
  and test-path exclusion.  Reference-kind filtering prevents call references
  from resolving to non-callable symbols.
- **Budget truncation**: plans exceeding remaining capacity are partially
  absorbed instead of wholesale rejected.
- **Strategy reorder**: `ImportNeighborhood` runs before `CallGraph` (correct for
  C/C++ include visibility).  Call-graph depth reduced to 1 with fixed-point
  iteration.
- **Bootstrap skip**: cold queries on persistent databases skip project-wide
  scanning when file inventory already exists.
- **Hot region tracking**: in-memory LRU eviction for visibility-filter
  deduplication.
- **`JobTracker`**: per-closure completion tracking with atomic snapshots.
- **Session cleanup**: previous-session control-plane rows cleared on project open.

### Resolution Improvements

- **Test/spec file exclusion**: `find_exact_name_target_in_scope` and
  `find_symbols_by_name` now exclude test/spec paths from global symbol scope.
- **Importer-aware relative import resolution**: lexical path normalization with
  extension/index resolution handles `./utils` → `src/utils.ts` correctly.
- **Edit-distance fuzzy fallback** skipped for call references — prevents
  `createWidget` from resolving to `createGadget` at callsites.

### Schema V2

- **28 entity tables + FTS5** in `.atlas/atlas.db`.  New focus control-plane
  tables: `closure_generations`, `closure_coverage`, `reference_resolutions`,
  `symbol_edge_candidates`, `file_inventory`, `symbol_hints`.
- Focus control-plane rows are transient session materialization; canonical
  source facts and symbol edges are durable in the same database.
- Schema comments corrected throughout the codebase (V3→V2).

### Language Capability

- **All 14 languages at DataflowFull** with ArgToParam + ReturnToCall summaries.
- **CFG**: 12 of 14 languages (ArkTS and PHP excluded).  CFG builder now emits
  `stmt_range` for branch and loop nodes across all supported languages.
- **Scope-exit analyzer**: `CallReturn` ownership test enabled — directly
  returned allocations (`return make_resource()`) are correctly recognized as
  ownership transfer to the caller.
- **FeatureMatrix**: DRY via `named_features()` — new fields no longer require
  three mirror lists.

### Infrastructure

- **Removed**: `atty` dependency; `SyncState` background sync subsystem;
  `ExecutionContext`; `analysis_response.rs`; `trace_mcp_e2e.rs` (1,964 lines).
- **`ClosureGraphProvider`**: `*const GraphState` + `unsafe impl Send/Sync` →
  `Arc<GraphState>`.
- **`Box<CallerChain>`** in `JobResult` for clippy `large_enum_variant`.
- **Rust idiom modernization**: inline format args, `matches!()`, `is_some_and()`,
  `Range::contains()` across the codebase.
- **Dossier**: peer endpoint selection fix, relation evidence deduplication,
  empty import symbol handling.
- **Trace locator**: falls back to `find_latest_visible_reference_target()` when
  a reference's `resolved` field is absent.
- **Filesync**: clean git repos return `Some(empty)` instead of `None`, preventing
  fallback to hash-based scanning.
- **`branch_diff`**: 0-based to 1-based line number conversion for human display.

### Documentation

- Architecture, requirements, roadmap, testing, and performance documents
  updated for Schema V2, TUI 2.0, and the current 15-tool MCP surface.
- All crate READMEs and the Agent SKILL definition synchronized.
- Removed outdated "multi-language extension roadmap" section — all languages
  now at DataflowFull with CFG per capability profile.

---

## [1.5.0] — 2026-06-15

### Focus Runtime — Query-Time Incremental Analysis (v5.0 → v7.1)

The largest architectural change since v1.0: a new query-time control plane that decides
_what_ facts to build, _in what order_, and _at what scope_ — replacing the old per-handler
ad-hoc lazy extraction with a unified, intent-driven pipeline.

- **`FocusRuntime` + `QueryIntent`** — single MCP entry point: handlers no longer directly
  orchestrate lazy structural/dataflow, resolver, or graph builder. Each MCP tool declares
  a `QueryIntent` (Calls, Path, Impact, etc.) and receives a prepared `FocusClosure`
  with scoped graph overlay, visibility filter, and capability contract.
- **`ClosureEngine`** — strategy-driven fixed-point closure expansion
  (`ImportNeighborhood` → `CallGraph` → `TypeGraph`) with `WindowBudget`-controlled
  iteration limits and per-step coverage tracking.
- **`BootstrapManager`** — cold-start tiered bootstrapping: Tier0 file inventory →
  Tier0.5 content fingerprints → Tier1 `SymbolHints` → Tier2 opportunistic manifest.
- **`ScopedResolver` + `FocusGraphBuilder`** — closure-scoped reference resolution
  and scoped graph overlay with per-generation staging (staged → visible transition).
- **`ProjectSlot`** — `Option<ActiveProject>` constrained to outermost MCP router layer;
  runtime structs receive `&ActiveProject` by reference only.
- **`RuntimeInvalidation`** — generation-based invalidation replaces signature comparison
  for graph snapshot freshness.
- **`GraphProvider` trait** — abstraction layer for graph queries, enabling a future
  closure-scoped `GraphState` that owns subset snapshots.
- **`AnalysisEnvelope`** — unified MCP response envelope replacing the legacy
  `LazyResponse` with structured `analysis` (state/scope/contract), `precision`
  (coverage tier + semantic confidence), and `work` (progress/items) sections.
- **Focus is transparent**: no CLI command, no manual warm-up, no user-visible surface.
  Activates silently when the project has no full index.

### Precision Model Migration

- **`PrecisionTier` → `Precision { coverage_tier, semantic_confidence }`**: the old
  6-variant enum replaced by a two-axis model (`CoverageTier` × `SemanticConfidence`)
  across the entire MCP contract. This is a **one-time public API break** — all MCP
  tool responses now carry the new `precision` field shape.
- Legacy `precision_tier` column removed from `closure_coverage` table (was always
  written as empty string, never read).
- `CapabilityMask` extended with `DATAFLOW`, `SUMMARIES` bits; lazy extraction state
  queries now gate on actual persisted facts, not just language capability profiles.

### Performance Optimizations

- **Resolution pipeline**: S1-S6 strategy counters and timers for observability;
  thread-local import resolution caches; per-file resolution fingerprints skip
  unchanged files; batch callsite callee backfill in Phase 2; project-level
  generation tracking for no-op index skip.
- **DB writes**: insert-only hot table writes in bulk batches; deferred index
  creation and FTS rebuild; WAL checkpoint observation; symbol dedup before write;
  per-phase wall-clock timing in `IndexPipelineStats`.
- **Search/context**: `levenshtein_bounded` with early termination; context banding
  for large result sets; proximity fuzzy search in S6.
- **Callsite denormalization eliminated**: `callsite.callee` column removed;
  callee resolution now derived from `ReferenceResolution` + graph edges at query
  time, eliminating a long-standing write-path denormalization.

### Extraction & Language Support

- **Cangjie adapter fixes**: added missing `is_identifier_decl_or_property` filter
  for `df.identifier_use` (was producing false-positive VariableUse DataNodes for
  declaration names); `imported_name` now correctly extracted from scoped identifier
  (was always empty string).
- **Extraction pool**: per-file thread isolation with 8 MiB stack for deep nesting
  (rayon-based custom extraction pool); later reverted in favor of CLI-level stack
  configuration.
- Tree-sitter-cangjie pinned to specific git rev for reproducible builds.

### Dead Code Subtraction (~4,000 lines)

A comprehensive 16-crate audit identified and removed dead code, duplicated logic,
unused dependencies, and unnecessary abstractions:

- **Dead code removed**: `AnalysisResponse` hierarchy (~300 lines, atlas-mcp);
  `ClosureGraphProvider` (93-line no-op pass-through); `FullRebuildGuard` struct;
  `CompositeProvider` (analysis); `IncludeGraph` module (resolution); `FrameworkResolver`
  + `ReactResolver` (resolution); `search::fts` + `search::fuzzy` modules; `LazyOutcome`
  type (engine facade); `WeightBudget` struct (filesync); `RuleStatus::Deprecated`
  variant (domain_rules); 30+ `#[allow(dead_code)]` methods and fields across all crates.
- **Dead schema removed**: `known_gaps` table (DDL only, zero read/write code);
  `precision_tier` column from `closure_coverage`.
- **Unused dependencies removed**: `hex` (atlas-mcp), `serde`+`derive` from 4 crates
  (resolution, search, filesync, graph), `workspace` from `lazy` crate.
- **`FocusJobState`** reduced to `Planned` only — 7 dead variants removed.
- **`LazyBudget`**: 8 dead methods/fields removed.

### Duplication Elimination (~2,500 lines)

- **Extraction adapters**: 3 new `make_df_*` helpers in `shared.rs` —
  `make_df_receiver_or_literal`, `make_df_assign_value`, `make_df_call_arg` —
  eliminating ~660 lines of near-identical dataflow arm code across 9-11 adapters each.
  `innermost_scope` + `contains_range` extracted from 3 independent implementations.
  `find_c_like_declaration_header` + `leading_parenthesized` deduplicated between
  C and C++ adapters.
- **DB store**: `batch_execute_chunked` low-level helper consolidates the repeated
  chunk/placeholder/param/execute pattern from 6 chunked INSERT functions (~115 lines).
  `batch_insert_edges` thin alias removed (migrated to `insert_edges`).
  `file_extraction_state` + `unit_extraction_state` merged into single module.
- **MCP handlers**: `ensure_cfg_for_function` on `AnalysisRuntime` (2 duplicated
  CFG lazy-loading blocks → 1). `format_ambiguous_error` helper (5 duplicated
  error-formatting blocks → 1). `validate_symbol_name_length` (14 duplicated validation
  blocks → 1). `include_roots` logging moved inside `include_roots_from_args`.
- **Analysis**: `read_file_lines` helper deduplicated snippet extraction boilerplate
  in trace engine. `add_chain_trail` for diagnostics. `domain_rules`/`rule_learning`
  thin re-export modules inlined into `lib.rs`.
- **Domain rules**: `display_name()` dead trait default removed. `explain_candidate()`
  and `discover_candidates()` added as trait defaults (removed 9 duplicate overrides
  + 9 empty stubs). 20 `test_validate_*` tests consolidated into single parameterized
  file. `builtin_rules()` construction pattern deduplicated across 10 language registries
  (`rules_from_static` helper).
- **Resolution/search**: `NameMatcher::name_similarity` now delegates to
  `search::compute_name_similarity` (gains prefix/CamelCase matching).
  `is_test_file` unified to use graph's comprehensive `is_likely_test_path`.
  `resolve_strategies_1_through_5` extracted as shared function (S1-S5 deduplicated
  between `resolve_one_core` and `resolve_one_scoped`).
- **Cross-function bridge**: shared param/callreturn traversal helpers extracted
  (`find_param_index`, `resolve_callsite_to_callee`) from `CrossFunctionBridge`
  and `SummaryEdgeProvider` fallback.
- **C/C++ resource rules**: builtin function name constants (`C_ALLOC_FUNCTIONS`,
  `C_FREE_FUNCTIONS`, `C_MAYBE_OWNED`) extracted as shared data source consumed
  by both `CppOwnershipRules` and `ResourceOpConfig`.

### Types & API Consistency

- **Missing root re-exports added**: `EffectKind`, `BoundaryMarker`, `BoundaryKind`,
  `ForwardChain`, `ForwardChainStep`, and all progress types (`ProgressPhase`,
  `ProgressState`, etc.) now available from `types::*` flat namespace.
- **`MANIFEST_BIT`/`STRUCTURAL_BIT`** duplicate constant aliases removed (13 callers
  migrated to canonical `MANIFEST`/`STRUCTURAL`).
- **`PhaseTiming`** renamed to `PipelinePhaseTiming` in filesync crate to avoid
  collision with `types::timing::PhaseTiming`.
- **`CapabilityProfile` data-declaration prototype**: Go and Python profiles migrated
  to `ProfileSpec` + `build_profile()` pattern; remaining 12 languages queued in roadmap.

### Bug Fixes

- `inventory_scope_child_bounds` fragile `"0"` upper bound replaced with canonical
  `char::MAX` (no data loss in practice, but wrong in principle).
- MCP runtime switched from `current_thread` to `multi_thread` so progress-forwarder
  runs during synchronous tool dispatch.
- Lock poison recovery unified: all `.unwrap()`/`.expect()` → `.unwrap_or_else(|e| e.into_inner())`.
- `PathAliasConfig::has_changed` check added before skip-resolution decision.
- Stale file cleanup now clears resolution fingerprints to prevent cross-file
  reference staleness.
- `detect_index_mode` hardened with focus-aware messaging and `partial_result` propagation.
- ASCII art doc block missing language tag fixed (was breaking doctest).
- `expand_types` silent skip hardened with `warn!` logging.

### Documentation

- **Focus architecture**: v5.0 architecture document (`docs/atlas-focus-architecture-v5.0.md`)
  covering Focus Runtime design, closure engine, bootstrap tiers, and Focus-Lazy
  boundary constraints.
- **Roadmap**: §10 added for code quality technical debt (ProfileSpec migration,
  FeatureMatrix mirror method merge).
- Deep-dive technical blog (`atlas-deep-dive.md`) covering index pipeline, MCP tools,
  lazy indexing, and semantic analysis.
- Performance optimization journey documented with methodology and verified results.
- Architecture diagram added to README.

---

## [1.4.2] — 2026-06-09

### SymbolSelector — Closed-Loop Symbol Resolution

A new fault-tolerant resolution engine that eliminates silent ambiguity across
all MCP tool handlers. When a symbol query matches multiple candidates,
tools now return scored candidate lists with `symbol_id` for deterministic
follow-up — no more silent wrong-symbol results.

- **`SymbolSelector` engine** (`engine` crate): multi-signal scoring
  (name match, file proximity, kind preference, scope overlap) with
  configurable thresholds and fallback strategies.
- **All 13 symbol-accepting MCP tools** now use `SymbolSelector` via
  `resolve_symbol_input()`, replacing direct `resolve_qname_disambiguated`.
- **Tool schemas** updated to `oneOf [string, SymbolSelector]` — callers
  can pass a plain string (backward-compatible) or a structured selector
  with `file_path`, `kind`, `scope` hints for disambiguation.
- **Candidate transparency**: ambiguous results include `candidates[]` with
  `symbol_id`, `name`, `kind`, `file_path`, `score` — callers pick the right
  one on retry.
- `ScoredCandidate.symbol_id` eliminates store round-trips for candidate lookup.
- Integration tests verify the full resolve → disambiguate → retry cycle.

### MCP Refactoring

**Module decomposition:**
- `ToolRouter` split into focused sub-modules: `GraphState`, `CacheState`,
  `AsyncState`, `symbol_selector.rs`, `lazy_response.rs`.

**LazyResponse unification** — all 7 lazy-triggering handlers now produce
responses through a single builder, eliminating per-handler field-drift:
- `handle_impact`, `handle_callees`, `handle_callers`, `handle_callgraph`,
  `handle_explore`, `handle_path`, `handle_index`.
- Every lazy response uniformly carries `precision_tier`, `hint`, `warnings`,
  `lazy_diagnostics`, `analysis_contract`, `query_id`, `QuerySnapshot`.

**Other MCP improvements:**
- `has_dataflow` and per-capability counts exposed in `project(action="status")`.
- Per-kind trace descriptions and position-based symbol lookup for `trace`.
- `ExploreDossierBuilder` for structured symbol exploration responses.
- Mutex poisoning recovery unified: all `.unwrap()` → `.unwrap_or_else(|e| e.into_inner())`.
- `generate_query_id()` fallback for misconfigured system clocks.
- `parse_symbol_input`: unified symbol argument parsing with stringified-JSON
  recovery. All handlers (`handle_symbol`, `handle_trace_caller_path`,
  `lifecycle`, `branch_diff`) now parse the `symbol` parameter through a
  shared entry point that handles strings, SymbolSelector objects, and
  accidentally-stringified JSON transparently.

### Extraction Deduplication

Shared helpers extracted from 11 language adapters into
`extraction/languages/shared.rs`:
- `make_binding_def`, `make_reference_use`, `make_scope_def`
- `make_data_node` family (dataflow node helpers)
- `find_call_expression` deduplicated across 11 adapters
- `node_range` copies and analysis mode parsing deduplicated

### Dead Code Subtraction (~1100 lines)

- Deleted `extraction/src/engine.rs` (−202 lines), 7 dead graph methods (−175 lines)
- Removed dead `cfg_nodes` columns (`effect_kind`, `target_field`, `callee_name`)
- Removed deprecated `begin_bulk_write`/`end_bulk_write`, dead `extract_one` CLI wrapper
- Removed dead aliases (`loaded_all_symbols`, `LoadedDomainRules`, `AliasTable::build`, `NoopSink`)
- Removed `SearchSession` struct, `indexed_scope_json`, `FilteredCounts` (CLI dead code)
- Purged `CandidateInfo`, `resolve_qname_disambiguated` (replaced by SymbolSelector)
- Removed JS adapter `_file_path` dead parameters, `DfIndex::source_edges` field
- Fixed 8 wrong `#[allow(dead_code)]` annotations on actively-called symbols
- Gated test-only APIs with `#[cfg(test)]` per §10.5

### Architecture Documentation

- **§10.5 Convergence Constraints**: deletion-before-abstraction, entry-layer
  orchestration, language adapter boundaries, trait default correctness,
  MCP lazy envelope single-build-path, facade compatibility, test-only API rules.
- **§2.10 Cleanup PR Guard Rules**: zero-production-call-site verification,
  happy-path + branch coverage, MCP lazy response field equivalence, stable
  facade compile-level compatibility.
- **Stability tier markers** on `atlas-engine` facade re-exports.
- **SymbolSelector architecture** documented in `docs/architecture.md`.

### Other Changes

- `SourceLookupFn` type alias → `SourceReader` trait with blanket `impl`.
- `validate_rule` extracted as `LanguageRuleKinds` trait default method.
- All 14 languages compile by default (`all-languages` feature gate removed).
- Context: `includeFilePeers` flag to skip file peers in context view.
- `SymbolDef.signature` extraction pipeline added across all languages.

### Bug Fixes

- TUI index mode display now uses canonical `store.read_index_mode()` (was
  rolling its own detection with inconsistent labels).
- Engine reports real progress during Resolution, HashCheck, SummaryBuild phases.
- Sync progress reporting improved with finer-grained phase updates.
- Dossier: `SourceRepository` signatures unified, error handling hardened,
  recommendations made context-aware.
- Removed arch violation: bare lock unwraps replaced with poison-safe patterns.
- **Code walk audit — 7 MCP handler fixes** (141 tests, 0 warnings):
  - `handle_symbol_by_position`: `view` parameter now correctly dispatches to
    detail/context/usages handlers when using `file_path`+`line` lookup (was
    always ignoring view and forcing detail).
  - `resume_task` / `handle_calls`: deduplicated 62-line dispatch logic into
    shared `CallsDispatch` enum + `resolve_calls_dispatch()` function,
    preventing future drift between initial call and resume paths.
  - `maybe_refresh_graph`: doc comments now clarify that
    `ensure_structural_for_files` and `ensure_structural_for_symbol_name`
    already call it internally — callers should not duplicate.
  - Progress channel: `symbol` and `trace` tools now create MCP progress
    channels, so `context` view and `trace(point)` progress notifications
    are delivered instead of silently dropped.
  - `handle_callees`: now calls both `ensure_structural_for_files` and
    `ensure_structural_for_symbol_name`, mirroring `handle_callers` for
    symmetric lazy extraction (closes C/C++ header/source edge gap).
  - `view=detail` now passes the full structured SymbolSelector (including
    `file_path`/`kind`/`line` hints) to `handle_symbol_detail`, fixing a
    regression where filtering fields were silently discarded.
  - Invalid `file_path` in SymbolSelector now produces an inline diagnostic
    in the ambiguous-symbol error message ("file_path '...' does not match
    any file in the project"), so callers know their hint was ignored.
- `file_dependencies`: incoming direction now returns complete results for
  TS/JS/Python relative imports (e.g., `./utils`). Engine scopes Path B
  extended from `kind='include'` to all `is_relative=1` imports with
  `.././` normalization and extension/index resolution. MCP "both"
  direction merged via shared `merge_edge_deps` helper.
- `search`: precision tier now derived from actual scope capability when
  structural data exists in a full index, fixing `Unavailable` tier on
  `is_manual_full` queries.

---

## [1.4.1] — 2026-06-05

### Index Precision Guard

Running `atlas index` or `atlas sync` on a rich-indexed database (structural/full) with
the default manifest analysis mode could silently discard capability. v1.4.1 adds a
runtime guard that detects and prevents accidental precision downgrades.

- **`index_precision` module**: `would_downgrade_index_precision()`,
  `is_rich_index_mode()`, `extraction_mode_name()`, `recommended_analysis_for()` for
  detecting and explaining precision downgrade risk.
- **`Store::read_index_mode()`** with `compute_index_mode()` logic that distinguishes 8
  states: `none`, `unknown`, `manifest`, `partial_structural`,
  `partial_structural+lazy`, `structural`, `structural+lazy`, `full`.
- **`Store::dominant_language()`** and **`dominant_language_in_scope()`** — language-aware
  scoped search that applies language-specific extraction when a scope is dominated by a
  single language.
- **`Store::derive_capability_for_files()`** — capability aggregation across multiple
  files with edge-aware `STRUCTURAL`/`CALL_EDGES` bits.
- **`guard_against_precision_downgrade()`** — shared runtime check wired into CLI
  `index`/`sync` commands and MCP entry points. Refuses to re-index with lower precision
  unless `--force-reindex` is explicitly passed.
- **`--force-reindex` flag** added to `atlas index` and `atlas sync` for explicit
  override of the safety guard.
- **TUI** simplified: `preserve_unusable_db()` now uses `read_index_mode()` instead of
  manual layer counting.
- **Coverage**: unit tests for all `compute_index_mode` states,
  `guard_rejects_manifest_downgrade`, `guard_rejects_structural_downgrade`,
  `derive_capability` aggregation, and E2E MCP tests for index refusal of default
  manifest downgrade.

### Release CI Hardening

- **Static musl linking**: Linux release targets switched from `-gnu` to `-musl`
  (`x86_64`, `aarch64`, `riscv64gc`). musl-tools installed on native runners with
  `CC_*=musl-gcc` env. Static binary verification via `readelf` check added.
- **riscv64 workaround**: temporarily pinned to GNU target (`riscv64gc-unknown-linux-gnu`)
  until musl toolchain for riscv64 is available.
- **GitHub Actions dependency upgrades**: `actions/checkout` v4→v6,
  `actions/upload-artifact` v4→v7, `softprops/action-gh-release` v2→v3. Removed
  deprecated `FORCE_JAVASCRIPT_ACTIONS_TO_NODE24` env. Windows runner updated to
  `windows-2025-vs2026`.

---

## [1.4.0] — 2026-06-05

### Breaking

- All 15 workspace crates bumped to 1.4.0.
- `CapabilityMask`: CFG and DATAFLOW bits are now orthogonal. `from_layers("dataflow")`
  no longer sets CFG. `best_capability_name()` returns "summaries" when SUMMARIES bit
  is present.

### Multi-language Semantics (M2–M6)

Atlas now understands resource lifecycle and scope-managed cleanup across **11 languages**
through language-specific domain rule registries and a unified scope-exit analyzer.

- **New domain rule registries** for all 11 DataflowFull languages: Go, Rust, Python,
  TypeScript, Java, C#, Kotlin, Ruby, PHP, C/C++, ArkTS, Cangjie. Each provides
  `alloc_fn`, `free_fn`, `cleanup_fn`, and language-specific `owned_pattern` rules.
- **`CallContext` enum**: language-agnostic call-site context annotations set by the CFG
  builder — `GoGoroutine`, `GoDefer`, `PythonWith`, `JavaTryWith`, `CSharpUsing`,
  `ReactEffectCleanup`, `RubyBlock`, `KotlinUse`.
- **`ScopeExitAnalyzer`**: unified intra-procedural scope-exit analysis. Computes
  `SemanticEffect` chains with Free at block boundaries for Rust `Drop`, C++
  destructors, Python `with`/`__del__`, Java try-with-resources, C# `using`/IDisposable,
  Kotlin `.use`, React `useEffect` cleanup returns, and Go `defer`.
- **New CFG primitives**: `CfgNodeKind::BlockExit` (synthetic block-boundary nodes),
  `DataNodeKind::CleanupReturn` (React effect cleanup return), `EscapeTarget::AsyncContext`
  (goroutine/coroutine escape).
- **`SemanticEffect` enriched**: `consumption_style` (explicit/deferred/context-managed),
  `description` (human-readable origin), `eligible_for_implicit_cleanup` (per-callee
  gating).
- **`OwnershipContract` extended**: `classify_escape()` for goroutine/coroutine escapes,
  `supports_implicit_scope_cleanup()`, `eligible_for_implicit_cleanup()`.
- **CFG support uplift**: C# (was unsupported → 0.72), Kotlin (→ 0.67), Ruby (→ 0.65).
  Cangjie CFG gap closed (was unsupported → 0.60 with body traversal). All languages
  now report CFG status via `supported_with_limitations` with consistent "body traversal
  implemented" annotation.
- **Cangjie** promoted to DataflowFull with CFG, updated FeatureMatrix, and feature-gated
  file extension registration.
- **12 languages** had interprocedural summary confidence floors raised after ArgToParam
  and ReturnToCall bridge verification.

### Pipeline Convergence

Index, sync, and auto-index now share a unified orchestrator pattern.

- **`ProgressSink` trait**: typed progress event abstraction. Each entry point
  (CLI/MCP/TUI) implements its own sink that translates pipeline events into
  native progress displays. `Send + Sync` for rayon safety.
- **`IndexPipeline`**: full index pipeline orchestrator. Replaces duplicated
  phase logic across CLI index, TUI auto-index, and MCP index handlers.
- **`IncrementalPipeline`**: incremental sync pipeline orchestrator. Replaces
  duplicated sync logic across CLI sync and MCP sync handlers.
- **`JobContext`**: unified context for long-running jobs — bundles ProgressSink,
  cancellation token, and optional task ID. Used by CLI, MCP, and TUI.
- **Size reduction**: CLI index 509→274 lines (−46%), TUI auto-index 394→181 lines
  (−54%), SyncEngine 328→145 lines (−56%).
- **Pipeline equivalence tests**: `pipeline_equivalence.rs` verifies that running
  `IndexPipeline` and the CLI's `run_index_pipeline` produces identical DB state
  (files/symbols/edges/summaries) for the same project.

### Capability-Aware Indexing

Dirty-check now respects the requested analysis mode.

- `build_dirty_set_for_mode(store, discovered, root, mode)` replaces `build_dirty_set`.
  A file is clean only when its content hash matches **and** the DB has fresh complete
  file-level `extraction_state` covering the mode's required capability.
- Hash-clean files with insufficient persisted capability are added to the dirty set —
  this enables `atlas index --analysis full` to upgrade a hash-clean manifest/structural
  DB without source changes.
- `file_has_fresh_complete_capability()`: DB-level query checking whether a file at a
  given content hash has all required capability bits.
- `optional()` replaces `map_err(warn).ok()` for metadata key queries — missing
  `last_index_time`/`last_sync_time` is normal empty-DB state, not an error.

### TUI UX

- **Background job system**: `JobManager` executes search and trace on a worker thread.
  `Esc` cancels running jobs. The TUI input remains responsive during long operations.
  Job results delivered via polling on each tick.
- **Instant startup**: TUI no longer blocks on `ensure_index_before_tui`. Starts
  immediately; auto-index runs in background.
- **`SearchSession`**: wraps `Engine` for lazy structural retry on empty manifest
  results, matching MCP `ScopedSearchService` semantics.
- **Command polish**: `truncate_str`, `project_root`, `--analysis` validation, locale
  fixes.

### ScopedSearchService

Shared search engine used by both MCP and TUI.

- **3-tier search**: FTS5 → exact name → LIKE substring fallback.
- **`SearchAnalysis` mode**: `Manifest` (no lazy), `Structural` (always trigger lazy
  on empty), `Auto` (trigger lazy for scopes ≤30 files).
- **Auto skips lazy** when structural data is already present — avoids redundant
  re-extraction.
- **Scope normalization**: strips `./`, `./`, trailing `/`, backslash normalization.
- Returns structured response: results, coverage, triggered_lazy flag, capability mask,
  precision tier, warnings.

### MCP & Storage

- **`storage='auto'`** (default): `open_project` reuses `.atlas/atlas.db` only when the
  DB reports a reusable index (via `read_index_mode`); otherwise opens an in-memory
  zero-footprint session.
- **`ToolCallContext`**: per-tool context with progress sink, cancellation, and task
  tracking. MCP handlers delegate to shared services.
- **Bounded `file_deps`**: scope-limited queries prevent unbounded traversal.
- **MCP router hardening**: `Cell→RwLock` for engine access, `Mutex<Engine>` for
  graph snapshots.

### Impact Analysis Fixes

- **Lazy structural trigger**: `handle_callers`, `handle_callees`, `handle_callgraph`,
  `handle_explore`, `handle_impact` now trigger lazy structural extraction before
  accessing the graph snapshot — fixes empty results after manifest-only index.
- **ArkTS extraction fix**: `@Component` decorator struct detection now uses
  word-boundary-aware scanning instead of `strip_prefix("struct")`.
- **Trace direction control**: `atlas_path` with `direction` parameter fixed for
  reverse provenance queries.

### Bug Fixes & Hardening

- **DB instrumentation**: 20+ previously silent error swallowing sites now properly
  logged via `tracing::warn`/`error`.
- **MCP stability**: blocking event loop (`std::thread::sleep` → `tokio::time::sleep`),
  poisoned `Mutex` recovery, cancellation panic-safety, input validation hardening.
- **Release-blocking P1 fixes**: bounded candidates for impact analysis, cancel wiring
  for async jobs, search delegation through `ScopedSearchService`, prewarm per-store
  guard.
- **Index reliability**: `build_all` deadlock fix, CLI sync `DoneGuard` lifetime fix,
  `FileLock` ownership clarification, prewarm per-store (not global), ripgrep binary
  path fixes, `scope_file_count` semantic fix.
- **Lazy extraction**: BFS dedup, interleaved budget check, string constants fix,
  `has_cfg` propagation, worker hang after budget exhaustion, lazy callsite remapping.
- **CFG fixtures**: `cfg_if_else` + `cfg_loop` golden fixtures for 11 languages,
  `with_lifecycle` for Python, `goroutine` for Go, `try_resource` for Java,
  `use_resource` for Kotlin, `using_dispose` for C#, `procedural_resource` for PHP,
  `scope_exit` for Rust.
- **Type system alignment**: `FeatureMatrix.cfg` and `supported_features` list now
  asserted consistent via compile-time tests. `CapabilityMask` layer → bit mapping
  verified. `CfgNode.call_context` properly serialized/deserialized.

### Documentation

- Architecture docs: updated module boundaries (ProgressSink, JobContext,
  ScopedSearchService, pipeline orchestrators), capability profiles, database schema,
  tool references.
- Requirements: re-index mode-awareness rules, optional metadata semantics.
- Temporary language evolution documents deleted — content folded into core docs.
- README and MCP skill definition synced with 18-tool API.

---

## [1.3.1] — 2026-06-03

### BREAKING: MCP tool refactor (33 → 18 tools)

- **No alias compatibility** — old tool names return "Unknown tool" error
- All tools use clean names without `atlas_` public prefix
- See `docs/architecture.md` §11.3 for the full MCP tool specification

### Tool merges

| Old tools | New tool |
|-----------|----------|
| `open_project`, `status`, `files`, `language_capabilities` | `project(action="open\|status\|files")` |
| `symbol`, `context`, `usages` | `symbol(view="detail\|context\|usages")`, `symbol` parameter |
| `callers`, `callees`, `callgraph`, `neighbors` | `calls(direction="incoming\|outgoing\|both", edge_kinds=[...])` |
| `trace_point`, `trace_variable`, `trace_forward`, `trace_caller_path` | `trace(kind="point\|variable\|forward\|callers")` |
| `dependencies`, `dependents` | `file_dependencies(direction="incoming\|outgoing\|both")`, `file_path` parameter |
| `annotate_fp_dispatch`, `list_fp_annotations`, `delete_fp_annotation` | `fp_dispatches(action="add\|list\|delete")` |
| `atlas_annotate`, `atlas_domain_rules`, `atlas_rule_learn` | `domain_rules(action="add\|list\|delete\|learn")` |
| `jobs`, `atlas_jobs` | `tasks` |
| `atlas_resume` | `resume_task` |
| `atlas_lifecycle` | `lifecycle` |
| `atlas_branch_diff` | `branch_diff` |
| `index`, `search`, `explore`, `path`, `impact`, `task_status`, `wait_for_task` | Unchanged (prefix-only removal) |

### Other changes

- `symbol(view="context")` now outputs structured JSON instead of Markdown
- `file_dependencies` uses `file_path` (no `file_id`)
- `trace(kind="callers\|forward")` `symbol`/`from`/`to` parameters auto-detect hex IDs vs qualified names
- `project(action="status")` always includes language capabilities (no `verbose` gate)

### Branch diff architecture

- `branch_diff` now documents the semantic analysis path as the default
  (`semantic=true`) for MCP callers.
- Semantic branch diff compares `EffectComposition` data instead of only legacy
  single-effect CFG annotations.
- Added structured `BranchDiffIssue` output internally, including asymmetry kind,
  severity, confidence, true/false branch summaries, and evidence-bearing field
  effect details.
- Preserved compatibility with legacy `BranchDiff` consumers by converting
  structured semantic issues back into the existing public result shape.

### Release hardening

- `cargo check --workspace --all-features`, `cargo test --workspace --all-features`,
  strict `cargo clippy --workspace --all-targets --all-features -- -D warnings`,
  and `cargo build --release -p atlas-cli --features all-languages,mcp` pass.
- Fixed all-features test compilation by importing `CapabilityMask` in lazy coordinator tests.
- Cleared Clippy release-gate warnings across CLI, MCP, graph, analysis, extraction,
  resolution, search, context, and domain-rules modules.
- Hardened MCP background task tracking against poisoned `Mutex` recovery.
- Removed unused deprecated `serde_yaml` from `atlas-cli` and `Cargo.lock`.
- Added package `repository`, `homepage`, and `documentation` metadata for
  `atlas-cli`, `atlas-engine`, and `atlas-mcp`.
- Updated README, MCP README, architecture, requirements, and Atlas skill docs to
  match the current 18-tool MCP API and DataflowFull language matrix.

---

## [1.3.0] — 2026-06-02

### TUI

- **Interactive TUI**: running `atlas` with no subcommand launches a ratatui-based
  symbol search and detail browser.  Tabbed detail panels (Overview → Callers →
  Callees → Peers → Source) with keyboard navigation.
- TUI auto-indexes on first launch; Ctrl+C during indexing cleanly exits.
- Progress bar shows accurate per-phase throughput via phase-elapsed averaging.

### Domain rules engine

- Language-agnostic `domain_rules` infrastructure: `GenericRuleEngine` with
  `LanguageRuleKinds` trait, `CppOwnershipRules` consumer, and auto-learning
  (`RuleLearningStrategy`).  Rules keyed by deterministic blake3 hash to prevent
  underscore-separated field collisions.

### Lifecycle & analysis

- `analysis/lifecycle.rs`: intra-procedural field-state tracking (Unknown → Assigned →
  Freed → Nullified → Escaped) with rule-backed proof mode.
- `analysis/branch_diff.rs`: sibling-branch side-effect comparison.
- CFG node effect annotation: `cfg_nodes.effect_kind` / `cfg_nodes.target_field`.
- `lifecycle_proof.rs`: `Safe` / `Suspicious` / `Incomplete` verdicts.

### MCP tools

- **Lifecycle**: `atlas_lifecycle(symbol, field)`, `atlas_branch_diff(symbol)`.
- **Domain rules**: `annotate_domain_rule`, `list_domain_rules`, `delete_domain_rule`,
  `approve_domain_rule`.
- **Query resume**: `resume(query_id)` continues a previous partial-result query.
- **AnalysisContract**: `safe_conclusions`, `unsafe_conclusions`, `refinement_jobs`
  in all MCP responses.
- `LazyOrchestrator` + `LazyRefreshQueue` for background graph refresh.
- `CancellationToken` with checkpoints in extraction for interruptible budget enforcement.
- `LazyBudget` with time + file-count constraints.
- Input validation: string length bounds, release-profile hardening.

### Engine

- `atlas-engine` facade exports `ContextView`, `CalleeDetail`, `CallerDetail`,
  `SymbolDef`.
- `CancellationToken` (`CancelCheck` trait): interruptible `extract_file_with_mode_cancellable`
  with CP1–CP6 checkpoints.
- `ClosurePlanner`: import/include-based dependency closure for lazy resolution.
- `ExtractionMode::Manifest` for CLI `--analysis manifest` early-return.
- `CapabilityMask` (u16 bitflags) in `extraction_state`.
- `query_id` atomic counter for resume support.

### Bug fixes

- Fix `domain_rules` ID collision (blake3 + `\xff` delimiter).
- Fix bare `source[start..end]` slice → `source.get(..).unwrap_or("")`.
- Fix `BulkWriteGuard` RAII for safety pragma cleanup on drop/panic.
- Fix `.ok()` silently swallowing DB errors → explicit `QueryReturnedNoRows` match.
- Fix FK guard row-decode failures silently dropped → `eprintln!` warning.
- Fix missing `Resolution` progress phase (`start_phase` before `resolve_all_parallel`).
- Fix `rayon::build_global()` panic when TUI starts after Ctrl+C in CLI index.
- Fix duplicate `layer` column in `find_symbols_by_file` SELECT.
- Fix `GraphSnapshot` doc: clarify `&mut self` write-side mutability.
- Fix broken test compilation and add secret-file patterns to `.atlasignore`.
- Fix worker hang after lazy budget exhaustion.

### Documentation

- Architecture doc: update table count 22→23, add `domain_rules` with language-agnostic description.
- Merge `domain-rules-amendment.md` and `task-lazy-experience.md` into architecture docs.
- Add `domain-rules-language-guide.md`.
- Update MCP tool count 27→28.

### Internal

- Workspace-wide clippy cleanup (`-D warnings` passes).
- Crate versions bumped to 1.3.0.
- MCP skill definition updated.

---

## [1.2.0] — 2026-05-30

### Lazy indexing

- **ResolutionSymbols layer**: lightweight extraction (symbols + imports + scopes, no
  references/dataflow/callsites) for import dependency resolution.
- **ClosurePlanner**: import/include dependency closure computation.
- **Linux augmentation**: `EXPORT_SYMBOL` / `initcall` / `SYSCALL_DEFINE` post-extraction
  enhancement for C.
- **LazyCoordinator**: centralised coordination of lazy structural, resolution_symbols,
  and dataflow extraction jobs with `extraction_jobs` table tracking.
- **Precision tiers**: `Exact` → `PartialExact` → `DegradedStructural` →
  `LocalDataflowOnly` → `ManifestOnly` → `Unavailable`.
- Graph refresh after lazy structural via `replace_files_in_place`.
- `include_roots` coverage for context and trace MCP tools.
- Shared `ensure_structural_for_files` helper across MCP handlers.

### Extraction

- **RecoverySpec** trait: post-extraction recovery for ArkTS structs. (Superseded by byte-stable `struct`->`class ` pre-parse normalization in `normalize_struct_keywords`; see `arkts.rs`.)
- ArkTS golden fixtures for struct declarations.

### Bug fixes

- Fix doc consistency: `include_roots`, prewarm cache guard, schema comments.
- Fix `PREWARM_RUNNING` flag leak in background prewarm.
- Fix lazy dataflow: extract structural facts before invalidation, wrap in atomic transaction.
- Fix `cfg_nodes` deletion scoped to file; remove broken file-level dataflow guard.
- Fix filesync: three correctness issues from code review.

### MCP

- `atlas_jobs` tool for active extraction job observability.
- Delta graph refresh after lazy structural writes.
- Handler-level regression tests for `include_roots` warnings.

---

## [1.1.0] — 2026-05-28

### Performance

- Resolution: pre-built contexts, lock-free progress via cloned `AtomicU64`, live rate
  display during Phase 1, `sync_channel` streaming Phase 1→2.
- Resolution: pre-computed `lower_names`, `O(1)` import index, `Arc<SymbolDef>` indexes
  (75% fewer heap copies), fuzzy + proximity result caches.
- Graph: preload symbol table in `build_all` — eliminates 315k DB queries.
- DB write: batch size increased 100→500; cleanup batch delete.
- Search: strip quotes in field values, use SQL `LIKE` for non-FTS paths.

### Features

- Multi-language callback detection: `detect_callback_registrations` with generic +
  per-language patterns (Go package prefix, Python decorators).
- `atlas_path`: direction, confidence, breakpoints, production-code preference.
- `atlas_callgraph` with caller/callee summaries.
- `includeCode` parameter for symbol/callgraph/explore tools.
- `atlas_explore` for neighbours grouped by edge kind.
- Function-pointer annotation CRUD: `annotate_fp_dispatch`, `list_fp_annotations`,
  `delete_fp_annotation`.
- AST-driven source extraction with weighted Dijkstra pathfinding.
- Cangjie: `manifest.scm`, CFG support, `@definition.entry` capture.
- Atomic lazy structural re-index and annotation bridging.

### Bug fixes

- Fix C pointer-typed struct fields not extracted; C struct field handling.
- Fix `atlas_path` lazy structural extraction with multi-SymbolId retry.
- Fix `read_symbol_source` return full file content instead of name-only.
- Fix lazy dataflow destroying pre-built full-index dataflow facts.
- Fix `rayon::build_global` idempotency via `Once`.
- Fix resolution: `mutex.lock().unwrap()` → poison-safe.
- Fix graph tests and callers/callees start-node exclusion.
- Fix derived capability profile alignment with static profile.
- Fix TUI: cursor positioning, progress area clearing, completion summary rendering.

### Documentation

- Consolidate architecture docs, align with code.
- Tool counts, MCP schema, FP dispatch annotation references updated.
- Project-internal-only call edge visibility documented.

### Cangjie

- `manifest.scm` for top-level declarations.
- CFG support.
- `@definition.entry` capture for `mainDefinition`.
- Documentation update.

---

## [1.0.0] — 2026-05-25

### First release

Atlas is a local-first semantic knowledge graph engine for LLM agents.  It parses
source code with tree-sitter, stores deterministic code facts in SQLite, and exposes
28 bounded MCP tools plus a CLI for agent-powered codebase navigation.

- 14 languages at DataflowFull capability level.
- 10-stage reference resolution with confidence scoring.
- In-memory graph snapshots with BFS/DFS traversal.
- Cross-function bridging via persisted function summaries (4 tables).
- CLI: `status`, `doctor`, `index`, `sync`, `files`, `mcp`.
- MCP: 28 stdio tools with lazy graph init, background task support, progress
  notifications.
- 14-Cargo-package Rust workspace, edition 2024, SQLite 22-table schema V1.
