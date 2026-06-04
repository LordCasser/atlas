# TEMP Language Evolution Review

Status: temporary review notes, append-only during this audit.

Audit target: `docs/TEMP_LANGUAGE_EVOLUTION_PLAN.md`

Scope: review the current multi-language updates from the user's entry points
through language detection, grammar/frontend selection, query normalization,
CFG/dataflow extraction, semantic effect composition, storage/consumer exposure,
and per-language parsing depth.

## 2026-06-03 Review Pass

### Initial Pipeline Map

- User-facing entry points are CLI/MCP/indexing paths, but language capability
  truth starts in `types/src/capability.rs`.
- Parsing/frontend entry points are `extraction/src/grammar.rs`,
  `extraction/src/languages/mod.rs`, and the per-language files under
  `extraction/src/languages/`.
- Deep parsing is split between `.scm` query captures, per-language
  normalization methods, `DataFlowBuilder`, `CfgBuilder`, `SemanticBinder`,
  `EffectComposer`, and `ScopeExitAnalyzer`.
- The plan's main review risk is that many sections now claim completed
  language semantics, while several implementation paths may only provide
  generic resource matching or function-exit cleanup rather than the precise
  language boundary semantics described in the acceptance criteria.

### Entry-Layer Findings

1. **Default/all-language discoverability is easy to misread from the usage
   side.** `Language` always contains all 14 variants, but `from_extension`
   gates Java/C/C++/ArkTS/Cangjie/Go/C#/Rust/PHP/Ruby/Kotlin behind cargo
   features, while `all_extensions()` unconditionally includes Java/C/C++/ArkTS
   extensions in its initial vector. Evidence:
   `types/src/enums.rs:24-40`, `types/src/enums.rs:86-114`,
   `types/src/enums.rs:125-145`. `LanguageRegistry::load_grammar()` and
   `create_frontend()` are separately feature-gated in
   `extraction/src/grammar.rs:55-130` and
   `extraction/src/languages/mod.rs:94-126`.
   Impact: from a user's perspective, "supported language" is not a single
   property. It depends on the binary's features, detection, grammar loading,
   and frontend creation all agreeing. The plan's consistency gate should also
   check feature-gated entry availability, not only capability/doc tables.

2. **CFG execution is still gated by legacy string lists, not the typed
   `FeatureMatrix`.** The extraction path runs CFG only if
   `frontend.capability.supported_features` contains `"cfg"`:
   `extraction/src/extract.rs:411-430`. Dataflow and lexical extraction use the
   slot capability objects (`frontend.dataflow.capability().is_supported()` and
   `frontend.lexical.capability().is_supported()`) at
   `extraction/src/extract.rs:324-409`.
   Impact: ADR-6 says `FeatureMatrix` is authoritative, but actual CFG
   execution still depends on a backward-compatible string list. A matrix/string
   drift can make docs/MCP say CFG exists while indexing silently skips CFG, or
   vice versa.

### Semantic-Layer Findings

3. **ADR-2 is not implemented as written: `OwnershipContract` has no
   `classify_boundary`.** The plan says the existing trait should be extended
   with `classify_boundary(...) -> Vec<SemanticEffect>`. Current code only has
   `classify_return`, `classify_consumption`, and `classify_escape`; boundary
   semantics are encoded indirectly in `CallContext` and
   `ScopeExitAnalyzer`. Evidence: `types/src/effects.rs:143-157`,
   `types/src/enums.rs:354-371`,
   `extraction/src/cfg_builder.rs:619-754`.
   Impact: a language can have a domain registry and resource patterns while
   still lacking the intended per-language boundary consumer. This especially
   matters for Kotlin `.use {}`, Ruby block resources, and coroutine/async
   boundaries because there is no trait hook that can classify these as
   language-specific statement/block boundaries.

4. **`ScopeExitAnalyzer` currently frees every unmatched allocation at function
   exit for every language that reaches `compose_effects`.** The comments say
   this primarily models Rust Drop and context managers, but the implementation
   collects all `Alloc` effects and emits a `Free` at the nearest `BlockExit`
   for context-managed nodes, otherwise at the function `Exit`.
   Evidence: `analysis/src/effect_composer.rs:272-273`,
   `analysis/src/scope_exit.rs:22-29`, `analysis/src/scope_exit.rs:73-120`.
   Since `branch_diff` and semantic impact call
   `ResourceOpConfig::default_for(lang)` for the actual symbol language,
   `compose_effects` can run for C, C++, Go, TypeScript, JavaScript, Python,
   Java, C#, Rust, PHP, Ruby, and Kotlin:
   `atlas-mcp/src/tools/branch_diff.rs:124-154`,
   `atlas-mcp/src/tools/graph.rs:1328-1336`,
   `analysis/src/resource_ops.rs:87-102`.
   Impact: for languages without automatic deterministic scope cleanup
   semantics, missing cleanup can be masked as a generated function-exit Free.
   This directly conflicts with plan acceptance items like "missing defer/close
   is visible" for Go and "missing cleanup across branches" for C/C++.
   Scope-exit cleanup needs a language/contract-level policy, not an
   unconditional post-pass.

5. **Scope-exit cleanup ignores returned/escaped resources.** The post-pass
   only tracks `Alloc` places and explicit `Free` places; it does not consider
   `Return`, `Escape`, or returned dataflow before generating a function-exit
   `Free`. Evidence: `analysis/src/scope_exit.rs:30-61`,
   `analysis/src/scope_exit.rs:73-120`. `EffectComposer` only creates escape
   effects through `classify_escape(callee, call_context)`, not through
   return-value flow from the function under analysis:
   `analysis/src/effect_composer.rs:230-244`.
   Impact: plan acceptance items such as Python "`f = open(); return f` reports
   escape to return value", Rust "`mem::forget(x)` is reported as ownership
   escape", and Go goroutine/channel escape need explicit no-auto-free handling.
   As implemented, an owned resource can be classified as escaped/returned and
   still receive a generated Free at function exit unless it also has a matching
   explicit Free place.

### Language Findings: TypeScript, JavaScript, ArkTS

6. **JavaScript CommonJS acceptance criteria are not implemented at the query
   entry.** The JavaScript frontend is a thin TypeScript wrapper and uses the
   TypeScript grammar/query set for definitions, references, imports, scopes,
   lexical bindings, and dataflow:
   `extraction/src/languages/javascript.rs:1-5`,
   `extraction/src/languages/javascript.rs:90-214`. The TypeScript import query
   captures `import_statement` and `export_statement` re-export forms, but has
   no `require(...)`, `module.exports`, or `exports.foo` captures:
   `extraction/queries/typescript/imports.scm:1-39`.
   Impact: ESM and TS-compatible syntax can enter the pipeline, but the plan's
   JavaScript-specific CommonJS fixtures/acceptance are not supported by the
   deepest parsing layer yet. Users indexing Node/CommonJS projects will see
   missing import/export graph facts rather than low-confidence CommonJS
   diagnostics.

7. **React cleanup detection is function-wide, not scoped to a specific
   `useEffect` boundary.** The TS query captures any `return_statement` whose
   child is an `arrow_function` as `df.react_cleanup_return`:
   `extraction/queries/typescript/dataflow_builder.scm:91-93`. `EffectComposer`
   then marks every `Free` effect in the enclosing function as Deferred when any
   `CleanupReturn` DataNode exists:
   `analysis/src/effect_composer.rs:256-269`.
   Impact: this can over-classify unrelated cleanup calls as React deferred
   cleanup. From a usage perspective, the result should say "function contains
   cleanup-return pattern" unless the call is actually scoped under the
   `useEffect` callback boundary.

8. **ArkTS is structurally useful but still TS-compatible fallback, not native
   ArkTS DataflowFull.** The ArkTS frontend uses TypeScript grammar and all
   TypeScript queries, with a recovery pass only for `struct` declarations found
   inside TS `ERROR` nodes:
   `extraction/src/languages/arkts.rs:1-11`,
   `extraction/src/languages/arkts.rs:72-199`,
   `extraction/src/languages/arkts.rs:202-260`. Its profile marks CFG
   unsupported but still advertises DataflowFull and interprocedural dataflow:
   `types/src/capability.rs:844-905`.
   Impact: this is acceptable only if every user-facing response includes
   grammar provenance and explicitly limits the claim to TS-compatible ArkTS.
   Component decorators/lifecycle/state decorators are not parsed as native
   semantic facts at the query layer.

### Cross-Language Callee Normalization Findings

9. **Resource rules and DataNode callee names use different naming contracts.**
   `EffectComposer` classifies calls using `DataNodeKind::CallTarget.name`, not
   `access_path`: `analysis/src/effect_composer.rs:183-190`. Many language
   normalizers either store only the final selector segment in `name` or set
   `access_path` equal to that segment:
   TypeScript stores `name = "close"` and `access_path = "obj.close"` for
   member calls (`extraction/src/languages/typescript.rs:458-484`);
   Python stores `name = "close"` and `access_path = "f.close"`
   (`extraction/src/languages/python.rs:288-314`);
   Go stores `name/access_path = "Open"` or `"Close"` for `os.Open` /
   `f.Close()` (`extraction/src/languages/go.rs:673-697`);
   Java/C#/Kotlin similarly set `access_path = name` for call targets
   (`extraction/src/languages/java.rs:523-548`,
   `extraction/src/languages/csharp.rs:528-550`,
   `extraction/src/languages/kotlin.rs:659-675`).
   But `ResourceOpConfig` expects fully qualified or dotted/suffixed forms such
   as `os.Open`, `.Close()`, `.close`, `File.open`, `File.Open`,
   `std::mem::forget`, etc.:
   `analysis/src/resource_ops.rs:149-185`,
   `analysis/src/resource_ops.rs:199-217`,
   `analysis/src/resource_ops.rs:227-247`,
   `analysis/src/resource_ops.rs:257-314`,
   `analysis/src/resource_ops.rs:317-404`.
   Impact: many "implemented" semantic resource rules will never fire on real
   DataNodes. Examples: Go `os.Open` normalizes to `Open`, so the exact
   `os.Open` producer misses; Go `f.Close()` normalizes to `Close`, so suffix
   `.Close()` misses; Python `f.close()` normalizes to `close`, so suffix
   `.close` misses; TypeScript `conn.close()` normalizes to `close`, so suffix
   `.close` misses. The callee contract needs to be unified: either classify on
   access_path first, normalize language call targets to canonical full callees,
   or have matchers check both name and access_path.

### Language Findings: Go, Rust, C, C++

10. **Go resource lifecycle acceptance is blocked by callee normalization and
    unconditional scope-exit cleanup.** Go query captures selector call targets
    as only the `field_identifier` (`Open` / `Close`) at
    `extraction/queries/go/dataflow_builder.scm:36-47`, and the normalizer uses
    that as both name and access_path:
    `extraction/src/languages/go.rs:673-697`. The rules require exact
    `os.Open`, `sql.Open`, `net.Dial`, and suffix `.Close()`:
    `analysis/src/resource_ops.rs:257-271`. Separately, even if an Alloc is
    produced, `ScopeExitAnalyzer` can auto-free it at function exit without a
    `defer` or `.Close()`.
    Impact: both sides of the Go acceptance criteria are at risk: correct
    `defer f.Close()` may not balance because the producer/consumer names miss,
    while missing close may be hidden by generated function-exit Free.

11. **C/C++ missing-cleanup semantics are at risk in semantic branch_diff.**
    Direct C calls such as `malloc`/`free` are more likely to match the c-like
    exact rules, but the generic `compose_effects` pass still auto-frees
    unmatched Allocs at function Exit. Evidence:
    `analysis/src/resource_ops.rs:105-140`,
    `analysis/src/scope_exit.rs:73-120`.
    Impact: the plan's C acceptance items around missing cleanup across
    branches and lifecycle proof mode should not rely on semantic branch_diff
    until scope-exit cleanup is restricted to languages/resources with real
    implicit release semantics. The separate `lifecycle` MCP path still uses
    `CppOwnershipRules`, but semantic branch_diff/impact use the generic
    ResourceOpConfig path.

12. **Rust M4a resource rules need verification against scoped call parsing.**
    Rust rules match exact `Box::new`, `Arc::new`, `Rc::new`,
    `std::mem::drop`, and `std::mem::forget`:
    `analysis/src/resource_ops.rs:281-314`. The Rust dataflow query only
    captures direct `(identifier)` calls and `field_expression` method calls:
    `extraction/queries/rust/dataflow_builder.scm:45-56`. It does not visibly
    capture `scoped_identifier` / path-style calls as a full callee. The
    normalizer then stores the captured node text as the call target name:
    `extraction/src/languages/rust.rs:586-666`.
    Impact: `Box::new(...)` and `std::mem::forget(x)` may fail before semantic
    classification because the deepest parsing layer does not provide the
    callee string expected by the rules. If existing tests pass, they may be
    exercising `ResourceOpConfig` directly rather than the full parse →
    DataNode → EffectComposer path.

### Language Findings: Java, C#, Kotlin, Ruby, PHP

13. **PHP and Ruby resource semantics are documented as completed, but their
    capability profiles still have no CFG path.** PHP and Ruby both advertise
    DataflowFull while marking CFG unsupported:
    `types/src/capability.rs:1175-1231`,
    `types/src/capability.rs:1235-1294`. `compose_effects` requires a CFG graph,
    and `branch_diff` reports CFG unavailable when CFG cannot be loaded:
    `atlas-mcp/src/tools/branch_diff.rs:147-165`.
    Impact: the plan's M6d text that `ScopeExitAnalyzer` handles PHP and Ruby
    resources at function exit is misleading from usage. Without CFG nodes,
    PHP/Ruby resource patterns can exist in `ResourceOpConfig`, but the semantic
    branch/lifecycle consumer path cannot use them as described. User-facing
    docs should say these are pattern definitions / future semantic inputs, not
    completed lifecycle semantics.

14. **Ruby block-managed resources are not represented as a block boundary.**
    Ruby dataflow captures calls, method calls, args, implicit returns, and
    field/index access, but there is no block/yield boundary capture or CFG:
    `extraction/queries/ruby/dataflow_builder.scm:20-76`,
    `types/src/capability.rs:1258-1264`.
    Impact: `File.open(...) { |f| ... }` cannot currently be proven as
    context-managed cleanup through the same BlockExit path used by Python,
    Java, and C#. The plan's acceptance "`File.open {}` Fixed" should be
    downgraded unless there is a separate non-CFG consumer path surfaced to
    users.

15. **Kotlin `.use {}` and coroutine semantics lack boundary plumbing.**
    Kotlin CFG supports if/loops, but `CallContext` has no Kotlin coroutine or
    Kotlin use/block context: `types/src/enums.rs:354-371`. The Kotlin dataflow
    query captures simple call targets only:
    `extraction/queries/kotlin/dataflow_builder.scm:42-49`, and the normalizer
    stores `access_path = name`:
    `extraction/src/languages/kotlin.rs:659-675`. The rule set expects exact
    `.use` and suffix `.close`/`.dispose`:
    `analysis/src/resource_ops.rs:390-404`.
    Impact: `.use {}` is currently a method-call pattern at best, not a
    context-managed block with a scoped exit. Coroutine capture-to-AsyncContext
    also has no CFG/CallContext producer, despite `EscapeTarget::AsyncContext`
    existing.

16. **Java/C# context managers have CFG nodes, but constructor/resource call
    matching still depends on the same callee-name contract.** CFG builder emits
    `CallContext::JavaTryWith` and `CallContext::CSharpUsing` and inserts a
    `BlockExit`: `extraction/src/cfg_builder.rs:619-659`,
    `extraction/src/cfg_builder.rs:675-707`. But Java/C# dataflow call targets
    are normalized to bare names/access_path:
    `extraction/src/languages/java.rs:523-548`,
    `extraction/src/languages/csharp.rs:528-550`, while rules expect suffix or
    exact patterns such as `newInputStream`, `getConnection`, `File.Open`, and
    `new FileStream`: `analysis/src/resource_ops.rs:227-247`,
    `analysis/src/resource_ops.rs:317-331`.
    Impact: try-with-resources/using BlockExit can exist structurally, but no
    context-managed Free is emitted unless the allocation call is first
    classified as an Alloc. End-to-end fixture coverage should assert full parse
    to semantic effect, not only CFG shape or ResourceOpConfig matching.

### Language Findings: Cangjie

17. **Cangjie CFG capability is internally inconsistent and likely disabled in
    extraction.** The `FeatureMatrix` marks Cangjie CFG as supported with
    limitations (`types/src/capability.rs:967-970`), but the legacy
    `supported_features` list omits `"cfg"` and uses older names such as
    `"local_dataflow"` / `"use_def"`:
    `types/src/capability.rs:913-927`. The extraction pipeline decides whether
    to run CFG using `supported_features.contains("cfg")`, not the matrix:
    `extraction/src/extract.rs:411-430`.
    Impact: Cangjie can advertise CFG support through the typed matrix while
    actual full extraction silently skips CFG. This is a concrete example of
    ADR-6 not being enforced. It also means the plan's "align capability
    profile, docs, and actual CFG behavior" item is still open.

18. **Cangjie method calls are captured, but only the terminal method name is
    normalized as the call target.** The query now captures
    `postfixExpression(fieldAccess(...), callSuffix)` method targets:
    `extraction/queries/cangjie/dataflow_builder.scm:19-36`, but the normalizer
    sets `access_path = name` for call targets:
    `extraction/src/languages/cangjie.rs:525-549`.
    Impact: this is sufficient for basic call counting and intra-language call
    target names, but it is not a full receiver-qualified call fact. If future
    Cangjie resource/user rules depend on receiver-qualified calls, it will hit
    the same callee-normalization issue as Go/Java/Kotlin.

### Test Coverage Findings

19. **Several "resource lifecycle" tests do not prove end-to-end parse →
    DataNode → rule classification → semantic effect.** Java and C# tests index
    code and verify CFG/BlockExit, then manually construct an `Alloc` effect
    before running `ScopeExitAnalyzer`:
    `crates/atlas-cli/tests/integration.rs:1789-1869`,
    `crates/atlas-cli/tests/integration.rs:1930-2010`. Ruby and PHP tests index
    fixture files but only verify symbol extraction, while separate tests check
    `ResourceOpConfig` patterns directly:
    `crates/atlas-cli/tests/integration.rs:2366-2413`,
    `crates/atlas-cli/tests/integration.rs:2425-2465`.
    Impact: these tests can pass even when real parser/dataflow call target
    normalization prevents resource rules from firing. The missing test gate is:
    given a source fixture, `compose_effects` should produce the expected
    `Alloc`/`Free`/`Escape` effects with the expected `ConsumptionStyle` and
    source CFG node.

20. **The shared golden fixture ladder in the plan is not complete in the
    current fixture tree.** The fixture tree contains many simple/import/call
    and CFG fixtures, plus selected resource fixtures, but several resource
    fixtures lack `.expected.json` golden outputs and/or only appear in
    integration tests. Evidence: `crates/atlas-cli/tests/fixtures/` currently
    has `csharp/using_dispose.cs`, `kotlin/use_resource.kt`,
    `ruby/block_resource.rb`, and `php/procedural_resource.php` without matching
    expected JSON files in the same listing.
    Impact: resource semantics are not locked by the same golden-output
    discipline as structural/CFG facts. This makes the plan's "Golden Fixture
    Ladder" acceptance weaker for the language semantics layer.

## 2026-06-03 Review Pass 2: Global Lazy / Non-Lazy Path Audit

Scope for this pass: re-review after the latest fixes from the user-facing
entry points down to per-language parsing/dataflow/CFG/semantic composition,
explicitly comparing non-lazy and lazy execution paths.

### Global Path Map

Non-lazy structural path:

1. `filesync/src/index_phases.rs:144-162` detects `Language` and constructs a
   `LanguageFrontend`.
2. `filesync/src/index_phases.rs:171-216` calls extraction with the configured
   mode.
3. `ExtractionMode::Structural` produces symbols/references/imports/scopes/
   lexical bindings/callsites/exports, but no dataflow or CFG:
   `extraction/src/mode.rs:54-60`, `extraction/src/mode.rs:86-106`.
4. Since dataflow/CFG are absent, resource semantics cannot be computed from a
   purely structural index.

Non-lazy full path:

1. The same index/sync entry points call `extract_file_with_mode`.
2. `ExtractionMode::Full` runs references, dataflow, use-def, CFG, binding-use
   scan, callsites, and callsite/data-node backfill:
   `extraction/src/mode.rs:73-77`, `extraction/src/mode.rs:97-110`.
3. Full dataflow is built at `extraction/src/extract.rs:369-409`; CFG is now
   correctly gated by the typed `FeatureMatrix` at
   `extraction/src/extract.rs:411-419`.
4. Full callsites are derived from references at
   `extraction/src/extract.rs:473-557`. The provisional byte-based
   `DataNode.callsite_id` values are then remapped to real `CallsiteId`s at
   `extraction/src/extract.rs:559-610`.

Lazy dataflow path:

1. `lazy/src/loader.rs:279-338` reads the structurally indexed file, verifies
   the content hash, and reruns extraction in `ExtractionMode::LazyDataflow`.
2. Lazy extraction reuses structural facts conceptually, but does not emit them:
   `extraction/src/mode.rs:62-71`, `extraction/src/extract.rs:683-699`.
3. Lazy dataflow and CFG are built with the same language extractors as Full,
   with window filtering at `extraction/src/extract.rs:353-367`,
   `extraction/src/extract.rs:398-405`, and
   `extraction/src/extract.rs:634-678`.
4. Lazy writes only partitioned dataflow/CFG/bindings back to DB:
   `lazy/src/loader.rs:340-410`.

### Findings

1. **[P0] LazyDataflow still breaks callsite/data-node joins for every
   language.** Lazy mode intentionally skips references and clears callsites:
   `extraction/src/mode.rs:86-89`, `extraction/src/extract.rs:683-699`.
   The only remap from provisional byte-based callsite ids to real structural
   callsite ids is built from the in-memory `callsites` vector:
   `extraction/src/extract.rs:589-610`. In LazyDataflow that vector is empty,
   so `CallArg` data nodes keep provisional ids. The lazy DB backfill then
   looks for nodes whose `dn.callsite_id == real_cs_id`:
   `db/src/store/unit_extraction_state.rs:304-312`, so it cannot match those
   lazy nodes. Downstream queries that join by real callsite id, such as
   `db/src/store/dataflow.rs:206-223`, will miss lazy argument nodes.
   This affects TypeScript/JavaScript, Python, Java, C/C++, Go, C#, Rust, PHP,
   Ruby, Kotlin, ArkTS, and Cangjie equally whenever semantic or summary logic
   depends on callsite-argument data nodes. Full mode is not affected because it
   derives callsites and remaps ids in the same extraction run.

2. **[P1] Lazy language coverage is narrower than non-lazy indexing.**
   Full/structural indexing uses the general frontend factory path, and the
   workspace feature list includes ArkTS and Cangjie:
   `crates/atlas-engine/Cargo.toml:55-58`,
   `crates/atlas-cli/Cargo.toml:55-60`. Lazy, however, uses a process cache
   hard-coded to TypeScript, JavaScript, Python, Java, C, Cpp, Go, CSharp,
   Rust, PHP, Ruby, and Kotlin only:
   `lazy/src/loader.rs:412-438`. `build_dataflow_for_file` errors when the
   cached frontend is missing: `lazy/src/loader.rs:290-292`.
   Result: ArkTS and Cangjie can be structurally/full indexed when compiled in,
   but lazy dataflow requests for those files fail before parser entry.

3. **[P1] Lazy capability masks claim CFG even when no CFG was built or the
   language does not support CFG.** Lazy writes
   `MANIFEST | STRUCTURAL | CALL_EDGES | CFG | DATAFLOW` unconditionally after
   a build: `lazy/src/loader.rs:186-201`. The prebuilt-cache shortcut does the
   same when it sees any data nodes: `lazy/src/loader.rs:250-265`.
   For PHP/Ruby/ArkTS and any language/profile where CFG is unsupported or the
   extraction returned empty CFG, unit state still advertises CFG. Full mode is
   now correctly gated by `FeatureMatrix`; lazy state metadata is not.

4. **[P1] Cangjie capability metadata is still inconsistent.** The runtime CFG
   gate now reads the typed matrix (`extraction/src/extract.rs:411-419`), which
   fixes the previous extraction skip. But Cangjie still omits `"cfg"` from
   `supported_features` while `features.cfg` is supported-with-limitations:
   `types/src/capability.rs:913-927`,
   `types/src/capability.rs:967-970`. Because `all-languages` includes
   Cangjie, the new consistency regression around
   `types/src/capability.rs:1681-1713` should fail under an all-language feature
   test unless the legacy list is also aligned. This is primarily user-facing
   metadata/test drift now, not the main runtime CFG gate.

5. **[P1] Scope-exit cleanup remains too broad for several languages.**
   `ResourceOpConfig` enables `implicit_scope_cleanup` for C/C++ and Python:
   `analysis/src/resource_ops.rs:109-145`,
   `analysis/src/resource_ops.rs:200-229`. `ScopeExitAnalyzer` then emits a
   synthetic Free for every unmatched Alloc at function exit unless it is a
   context-managed block exit: `analysis/src/scope_exit.rs:29-116`. It does
   not account for `Return`/`Escape` effects when deciding whether a value is
   still owned by the function. This can mask real leaks in C and plain Python
   code, and can incorrectly mark returned resources as freed. This affects
   both Full and Lazy paths because both feed the same semantic composer when
   dataflow/CFG are available.

6. **[P1] ADR-2 boundary classification is still not implemented as a semantic
   contract.** `OwnershipContract` has `classify_return`,
   `classify_consumption`, `classify_escape`, and
   `supports_implicit_scope_cleanup`, but no `classify_boundary`:
   `types/src/effects.rs:143-165`. Context-managed cleanup is still inferred
   later by CFG `CallContext` plus the broad scope-exit pass:
   `analysis/src/effect_composer.rs:272-276`,
   `analysis/src/scope_exit.rs:37-46`. This leaves Python `with`, Java
   try-with-resources, C# `using`, Kotlin `.use`, Ruby block resources, and
   async/coroutine boundaries without one explicit language contract surface.

7. **[P2] Callee normalization is only partially fixed across languages.**
   TypeScript/JavaScript and Go now preserve receiver-qualified member/selector
   call names well enough for suffix rules (`conn.close`, `os.Open`, etc.).
   Several languages still pass terminal names or constructor type names into
   `EffectComposer`, which only reads `DataNodeKind::CallTarget.name`:
   `analysis/src/effect_composer.rs:183-188`.
   Per-language status:

   - TypeScript/JavaScript: improved; member-expression normalization now walks
     to the full member expression, so suffix consumers such as `.close` and
     `.dispose` can match.
   - Go: improved; selector expressions can preserve full text such as
     `os.Open` and `f.Close`.
   - Python: still weak for receiver-qualified calls. Rules expect
     `socket.socket`, `sqlite3.connect`, and suffix `.close`, but the call
     target normalizer still uses the terminal attribute name for matching.
   - Java: method calls are mostly terminal names; object creation captures
     type names, so `new FileInputStream(...)` style constructors do not
     naturally match suffix rules such as `newInputStream`.
   - C#: object creation captures a type such as `FileStream`, while the rule
     set includes exact `new FileStream`; constructor resource matches can
     miss.
   - Rust: rules expect exact paths such as `Box::new` and
     `std::mem::forget`, but the query path mainly captures identifiers/fields,
     so scoped paths may not reach the semantic matcher.
   - PHP: procedural resources such as `fopen`/`fclose` are likely OK; method
     receiver-qualified resources remain limited.
   - Ruby: terminal method names are not enough for rules such as `File.open`
     and block-resource boundaries; also no CFG means branch/scope semantics
     are limited in both lazy and full.
   - Kotlin: terminal names and call-expression query shape are not enough to
     model `.use {}` as a scoped resource boundary.
   - ArkTS: follows the TypeScript adapter, but lazy cannot currently enter it
     because the lazy frontend cache omits ArkTS.
   - Cangjie: method calls are captured, but lazy cannot currently enter it;
     receiver-qualified call semantics remain basic.

8. **[P2] React cleanup remains function-wide.** If any `CleanupReturn`
   DataNode exists, every Free effect in the function is marked Deferred:
   `analysis/src/effect_composer.rs:256-269`. This is independent of lazy vs
   full extraction. It may be acceptable for narrow React hook fixtures, but it
   is not yet tied to the actual returned cleanup closure or to the specific
   resource consumed inside that closure.

### Lazy vs Non-Lazy Summary By Language

- TypeScript/JavaScript: Full path now benefits from improved member-call
  normalization and CFG. Lazy path uses the same extractor but loses callsite
  joins due provisional ids; React cleanup over-applies function-wide in both.
- Python: Full and lazy share the same parser/dataflow/CFG, but receiver
  qualified resource calls and broad implicit cleanup remain risky. Lazy also
  has the global callsite-id problem.
- Java: Full/lazy can build CFG and try-with-resources block exits, but
  constructor/resource call classification is still name-contract sensitive.
- C/C++: Full/lazy direct C resource calls can be classified, but
  `implicit_scope_cleanup=true` is too broad for C and raw allocations.
- Go: Full/lazy selector call names are improved; deferred close style depends
  on CFG `CallContext::GoDefer`. Lazy still loses callsite arg joins.
- C#: Full/lazy can model `using` CFG, but constructor name matching is still
  weak.
- Rust: Full/lazy can use CFG and implicit Drop, but scoped callee paths such
  as `Box::new`/`std::mem::forget` remain fragile.
- PHP: Full/lazy dataflow can classify simple procedural resources, but CFG is
  unsupported; lazy state may still advertise CFG incorrectly.
- Ruby: Full/lazy have limited resource semantics because CFG is unsupported
  and block-resource boundaries are not modeled.
- Kotlin: Full/lazy CFG/dataflow exist, but `.use {}` is not yet a first-class
  boundary.
- ArkTS: non-lazy can enter through the TS adapter; lazy currently cannot enter
  because the frontend cache omits ArkTS.
- Cangjie: non-lazy can enter when the feature is compiled and CFG now uses the
  typed matrix; lazy currently cannot enter because the frontend cache omits
  Cangjie. Capability metadata still needs `"cfg"` alignment.

### Suggested Verification Gates

1. Add a lazy-specific fixture that indexes structurally, triggers
   `LazyDataflowLoader::ensure`, and asserts that callsite `args_json` contains
   `data_node_id` for TypeScript or Go.
2. Add one all-language capability consistency test command to CI, including
   Cangjie when the feature is enabled.
3. Add semantic end-to-end tests from source fixture to `compose_effects` for
   each resource language, not only direct `ResourceOpConfig` matching.
4. Add lazy coverage for ArkTS/Cangjie or explicitly exclude those languages
   from lazy with a capability/status reason.

## 2026-06-03 Review Pass 3: Fix Plan Critique

This section reviews the pasted repair plan critically against the current code
shape. Overall direction is sound: it groups related fixes by architecture
layer instead of chasing isolated symptoms. I would not execute it verbatim,
however. Several items need narrower contracts or a different implementation
route.

### Confirmed Strong Points

1. The plan correctly identifies `FeatureMatrix` as the intended authority for
   feature support. Current code already has
   `FeatureMatrix::supported_feature_names()` and
   `unsupported_feature_names()` (`types/src/capability.rs:187-229`), and
   `LanguageFrontend::derive_capability_profile()` already derives string lists
   from the matrix (`extraction/src/frontend.rs:450-460`). The right fix is to
   make hand-written `LanguageCapabilityProfile` construction use that existing
   derivation path, not add a parallel `active_features()` API.

2. The plan correctly treats lazy callsite remapping as the P0 functional bug.
   Lazy mode skips references/callsites and clears structural outputs, while
   the only callsite-id remap depends on the in-memory `callsites` vector.
   Result: lazy data nodes keep provisional `CallsiteId::from_file_byte` ids.

3. The plan correctly calls out the lazy frontend cache as duplicated language
   registration. Current lazy cache is hard-coded and omits ArkTS/Cangjie:
   `lazy/src/loader.rs:412-438`.

4. The plan correctly calls out unconditioned lazy capability masks. Lazy writes
   `CFG | DATAFLOW` regardless of actual extraction result:
   `lazy/src/loader.rs:186-201`, `lazy/src/loader.rs:250-265`.

5. The plan correctly says current resource tests are not enough. Direct
   `ResourceOpConfig` pattern tests do not prove source parse -> DataNode ->
   CFG -> `compose_effects`.

### Corrections Required Before Implementation

1. **Do not implement lazy callsite remap by simply "extracting callsites" in
   LazyDataflow.** In current extraction, callsites are derived from
   `ReferenceKind::Call` records (`extraction/src/extract.rs:473-557`), not
   from an independent cheap callsite pass. Turning on callsites in lazy
   implicitly means turning on references and caller binding, or creating a new
   callsite-only extractor with enough caller context. The lower-risk design is
   to load existing structural callsites from the DB and build a
   `provisional_id -> real_callsite_id` map from `callsite.range.start_byte`.
   Lazy should reuse structural facts, not rederive and risk diverging from
   them.

2. **Be careful with `CapabilityMask::from_feature_matrix(fm, has_data_nodes,
   has_cfg_nodes)`.** If the mask means "this unit has available persisted
   facts", then using actual counts is appropriate. If it means "this language
   is capable of producing the layer", then `has_data_nodes == false` is not
   enough to clear DATAFLOW because an empty function can be a successful
   dataflow build. The plan needs to define mask semantics first. Current bug is
   clear for CFG false positives; the fix should distinguish "supported",
   "attempted", and "facts present" instead of only checking non-empty vectors.

3. **Do not make `DataNode.name` universally equal to the full expression
   text.** `EffectComposer` currently reads only `DataNode.name`
   (`analysis/src/effect_composer.rs:183-188`), but `DataNode` also carries
   `access_path`. A cleaner contract is:
   - `name`: stable canonical callee identifier, without arguments.
   - `access_path`: receiver-qualified canonical path when available.
   - matcher input: try `access_path` first, then `name`.
   This avoids unstable names like `new FileStream(path, mode)` and avoids
   making generic syntax such as `Box::<T>::new` impossible to match with a
   stable pattern.

4. **Constructor normalization should not use raw expression text.** The plan's
   "new FileStream(...)" proposal is too brittle. Normalize constructors to a
   canonical callee such as `new FileStream` or `FileStream`, with the exact
   chosen form documented and tested. Arguments and type arguments should not be
   part of the resource matcher key.

5. **C/C++ scope cleanup must split resource kinds, not just language.** Setting
   C `implicit_scope_cleanup=false` is correct. Keeping C++ `true` is still too
   broad if the same C-like producers (`malloc`, `fopen`) are classified under
   C++. C++ RAII objects can scope-clean; C APIs and raw allocations cannot be
   blindly freed at function exit. The safer fix is either separate C and C++
   configs or resource patterns annotated with cleanup policy.

6. **Python `__del__` should not justify broad implicit cleanup.** Plain
   `f = open(...)` is not equivalent to `with open(...)`. Scope cleanup should
   be triggered by explicit boundary context (`PythonWith`), not by language
   default. Otherwise returned or leaked plain Python resources can be marked as
   freed.

7. **`classify_boundary(context)` is directionally good but underspecified.**
   The boundary contract should say what is consumed, at which boundary, with
   which `ConsumptionStyle`, and whether non-local exits are covered. A method
   returning only `BoundaryContract` from `CallContext` is too coarse unless
   the contract can inspect the allocation/callee and the CFG boundary node.

8. **React cleanup scoping needs a data model, not just byte-range filtering.**
   "Find the containing useEffect call" is plausible, but the code must prove
   the cleanup function's body range, the consumed resource, and the containing
   callback are linked. Otherwise it can still mark unrelated frees inside the
   same callback or miss nested cleanup closures.

9. **CommonJS and ArkTS documentation are valid tasks but not prerequisites for
   the P0/P1 fixes.** They should not be mixed into the critical lazy/resource
   repair batch unless the target milestone explicitly includes import coverage
   and ArkTS public capability messaging.

### Recommended Implementation Order

1. Add a failing lazy regression test for callsite arg `data_node_id` backfill.
   Fix it by remapping lazy data nodes against existing structural DB
   callsites, not by re-extracting structural facts in lazy.

2. Fix lazy frontend registration by introducing one shared compiled-language
   list in `types` or `extraction`, then use it from lazy and frontend tests.

3. Fix lazy capability mask semantics with tests for a CFG-unsupported language
   and an empty-but-successful dataflow unit.

4. Align capability profile string lists using the existing
   `FeatureMatrix::supported_feature_names()`/`unsupported_feature_names()`
   helpers. Preserve or intentionally migrate legacy names such as
   `local_dataflow` vs `intra_statement_dataflow` so external status/MCP output
   does not break silently.

5. Change `EffectComposer` matching to use a canonical candidate list
   (`access_path`, `name`) before editing every language normalizer. Then
   normalize per language with fixture-backed tests.

6. Split scope cleanup policy by explicit boundary/resource kind. Start with
   C=false and Python plain open=false; add C++ RAII only when the allocation
   pattern is known to be RAII.

7. Add end-to-end semantic fixtures per language only after the matcher and
   boundary contracts are stable; otherwise the tests will lock in unstable
   callee strings.

## 2026-06-03 Review Pass 4: Coverage Gaps Across Analysis Levels

This pass answers whether the prior review covered all code paths from basic
through structural to full. Short answer: not completely. The extraction core
and language adapters were reviewed heavily, but several orchestration and
state-reporting paths still need targeted review.

### Actual Level Model In Code

There is no standalone `ExtractionMode::Basic`. The project has two different
"level" concepts that must not be conflated:

1. Extraction modes:
   `Manifest`, `ResolutionSymbols`, `Structural`, `LazyDataflow`, `Full`
   (`extraction/src/mode.rs:36-77`).
2. Capability/precision tiers:
   `CapabilityLevel::{Symbolic, DataflowBasic, DataflowFull}` in capability
   profiles, plus lazy `PrecisionTier::{ManifestOnly, LocalDataflowOnly,
   DegradedStructural, PartialExact, Exact}` (`types/src/structs.rs:161-209`).

Prior review mostly covered `Structural`, `LazyDataflow`, `Full`, and the
resource semantic layer. The missing review surface is the glue between these
levels.

### Paths Already Reviewed Enough

1. Extraction phase switches in `ExtractionMode` and `extract.rs`.
2. Full vs lazy dataflow/CFG construction.
3. LazyDataflow callsite-id remap issue.
4. Per-language dataflow call target normalization.
5. Resource effect composition and scope-exit behavior.
6. Lazy frontend cache and lazy unit capability mask.

### Paths Still Needing Review

1. **CLI `atlas index` has a separate orchestration path from the shared
   filesync pipeline.** The previous review focused mostly on
   `filesync/src/index_pipeline.rs` and `filesync/src/index_phases.rs`, but
   the actual CLI command has its own discovery, frontend init, parallel
   extraction, cleanup, write, resolution, graph, summary, and finalization
   sequence in `atlas-cli/src/commands/index.rs:121-382`. Any mode-level fix
   must be validated through both:
   - `atlas-cli/src/commands/index.rs:39-43`, `218-235`, `312-325`,
     `327-371`
   - `filesync/src/index_pipeline.rs:82-218`

2. **`atlas sync --analysis ...` is a third mode entry.** Sync maps
   `manifest/full/default(structural)` at
   `atlas-cli/src/commands/sync.rs:12-17`, then calls
   `SyncEngine::with_mode` and `reindex_file`:
   `filesync/src/sync_engine.rs:24`,
   `filesync/src/sync_engine.rs:209-234`. It always proceeds to resolution and
   graph building after re-extraction (`sync_engine.rs:155-181`), even when
   mode is manifest. That means manifest sync can run resolution/graph phases
   with no references. This may be harmless but should be explicitly reviewed
   and tested.

3. **LazyStructural path was not fully reviewed.** Manifest-only projects can
   upgrade individual files through `LazyStructuralService`, which re-extracts
   `ExtractionMode::Structural`, replaces facts with invalidation, then runs
   incremental resolution/build:
   `atlas-engine/src/lazy_structural.rs:421-463`,
   `atlas-engine/src/lazy_structural.rs:483-588`,
   `atlas-engine/src/lazy_structural.rs:591-596`.
   This is the bridge from "basic/manifest" to "structural" for query paths and
   deserves its own review for stale hashes, path resolution, capability mask,
   and summary invalidation.

4. **High-level trace and raw trace take different dataflow paths.**
   `Engine::trace_variable` gates capability, triggers lazy dataflow, and then
   delegates to `TraceEngine`:
   `atlas-engine/src/lib.rs:330-417`. Raw `TraceEngine::trace_variable`
   does not trigger lazy dataflow; it only consumes existing DB data:
   `analysis/src/trace/engine.rs:197-277`. TUI and many tests use
   `RawTraceEngine` directly. That is fine as a low-level API, but user-facing
   tools must be audited to ensure they call the high-level engine when lazy
   dataflow is expected.

5. **Full mode writes dataflow/CFG facts but records the file layer as
   `"structural"`.** `extract.rs` returns `"dataflow"` only for
   `LazyDataflow`; every non-lazy mode after early returns, including `Full`,
   becomes `"structural"`:
   `extraction/src/extract.rs:727-732`. `write_file_facts` then records the
   extraction state mask from that layer string:
   `db/src/store_writers.rs:709-731`. Since
   `CapabilityMask::from_layers("structural")` only sets MANIFEST+STRUCTURAL
   (`types/src/structs.rs:745-754`), a full index can persist data_nodes/CFG
   while file-level extraction_state does not advertise DATAFLOW/CFG. Lazy
   later detects prebuilt dataflow by counting data nodes per unit, but status,
   capability analytics, and user-facing precision may underreport full mode.

6. **CapabilityMask semantics are still inconsistent across file and unit
   levels.** File-level masks are derived from layer strings:
   `types/src/structs.rs:745-770`. Unit-level lazy masks are manually assembled
   and currently overreport CFG/DATAFLOW. Capability counts treat fresh
   file-level `dataflow` layers specially:
   `db/src/store/file_extraction_state.rs:142-218`. This whole state model
   needs a separate review after deciding whether masks mean "capability",
   "attempted layer", or "facts actually present".

7. **ResolutionSymbols mode has only been lightly touched.** It extracts all
   symbols/imports/scopes and returns layer `"resolution_symbols"`:
   `extraction/src/extract.rs:287-321`. Store replacement for this layer has
   bespoke logic in `db/src/store/mod.rs:401-464`. It is not part of the main
   user-facing "basic -> structural -> full" ladder, but it affects dependency
   resolution and stale-layer behavior.

8. **LazyDataflow documentation and implementation differ on reused structural
   facts.** The mode table says LazyDataflow reuses symbols/imports/scopes, but
   current code still extracts definitions/imports/scopes/lexical data before
   clearing structural outputs:
   `extraction/src/extract.rs:130-256`,
   `extraction/src/extract.rs:324-348`,
   `extraction/src/extract.rs:683-699`. This may be necessary for function-id
   assignment and FK safety, but the contract should be made explicit.

### What To Review Next

1. Full-index state correctness: `ExtractionMode::Full` -> DB facts ->
   extraction_state -> status/capability APIs -> lazy prebuilt cache.
2. Manifest -> LazyStructural -> Structural upgrade, including summaries and
   invalidation.
3. Public tool routing: ensure MCP/TUI/search/context/trace use the right
   high-level engine when lazy structural/dataflow is expected.
4. Sync mode behavior for `--analysis manifest` and `--analysis full`.
5. ResolutionSymbols stale-layer and mask semantics.

## 2026-06-03 Review Pass 5: Post-Fix Full/Lazy/CLI/MCP/TUI Audit

Scope: the implementation has been updated after the earlier review. This pass
re-checks user-facing entry points and every language path from the entry to the
deepest dataflow/CFG/resource parsing layer. It explicitly separates Full
non-lazy and Lazy paths.

### Entry-Point Path Map

1. CLI index:
   `atlas-cli/src/commands/index.rs:39-43` maps `manifest`, `full`, and the
   default structural mode into `ExtractionMode`. Full then reaches dataflow
   at `extraction/src/extract.rs:369-390` and CFG at
   `extraction/src/extract.rs:411-421`.

2. CLI sync:
   `atlas-cli/src/commands/sync.rs:12-17` uses the same mode names and calls
   `SyncEngine::with_mode`. The engine now correctly skips resolution/graph
   work when the mode does not produce references:
   `filesync/src/sync_engine.rs:155-186`.

3. MCP index/search/context/trace:
   MCP `index` intentionally rejects an `analysis` argument and always builds
   the manifest layer (`atlas-mcp/src/tools/index.rs:44-47`). Deeper work is
   query-triggered. `ToolRouter` now constructs the high-level `Engine` in both
   constructors (`atlas-mcp/src/tools/mod.rs:156-165`,
   `atlas-mcp/src/tools/mod.rs:192-200`). `trace_variable` now calls
   `Engine::trace_variable`, which triggers lazy dataflow before delegating to
   raw trace (`atlas-mcp/src/tools/trace.rs:248-252`,
   `atlas-engine/src/lib.rs:377-444`).

4. TUI:
   TUI still only exposes caller tracing through `RawTraceEngine::trace_callers`
   (`atlas-cli/src/tui/app.rs:348-352`). It does not currently enter
   variable/dataflow trace. That is acceptable for the current TUI surface, but
   it means the new lazy dataflow path is not covered by TUI behavior.

### Findings

1. **[P1] File-level `dataflow` state still overreports CFG for CFG-unsupported
   languages.** The Full-mode layer bug was fixed: `extract.rs` now records
   `"dataflow"` whenever `mode.produces_dataflow()` is true
   (`extraction/src/extract.rs:727-732`). But `write_file_facts` still derives
   the stored file mask from the layer string only
   (`db/src/store_writers.rs:709-731`), and
   `CapabilityMask::from_layers("dataflow")` unconditionally sets CFG
   (`types/src/structs.rs:758-764`). Result: `atlas index --analysis full`
   can advertise CFG at the file level for PHP/ArkTS or any language/profile
   whose `FeatureMatrix.cfg` is unsupported, even though extraction correctly
   skips CFG at `extraction/src/extract.rs:411-419`.

2. **[P1] MCP `trace_variable` reports structural-only `lazy_diagnostics` even
   after it triggers lazy dataflow.** The engine performs lazy dataflow and
   attaches a `LazySummary` only when a trace path exists
   (`atlas-engine/src/lib.rs:390-443`). The MCP wrapper then overwrites the
   top-level `lazy_diagnostics` and `analysis_contract` from
   `LazyDiagnostics::from_structural` only
   (`atlas-mcp/src/tools/trace.rs:253-285`). The combined constructor already
   knows how to merge a `LazyWindow`, but it expects
   `LazyWindow.capability_mask` (`atlas-mcp/src/tools/lazy_response.rs:238-247`);
   `LazyDataflowService` never populates that field
   (`lazy/src/lib.rs:51-81`, `types/src/lazy.rs:99-105`). This makes MCP
   clients see an analysis contract that can omit the just-built dataflow/CFG
   layer or its pending/truncated state.

3. **[P1] Cangjie capability metadata still fails the new consistency contract.**
   Cangjie has `features.cfg` supported-with-limitations
   (`types/src/capability.rs:967-970`), but its legacy `supported_features`
   list does not include `"cfg"` (`types/src/capability.rs:917-926`). The new
   regression test explicitly asserts matrix/list consistency across compiled
   languages (`types/src/capability.rs:1684-1715`), so an all-language test with
   Cangjie enabled should fail unless the legacy list is aligned.

4. **[P1] Kotlin `.use {}` CFG classification is overbroad in both Full and
   Lazy paths.** The CFG builder marks any Kotlin `call_expression` with a
   recursive `lambda_literal` descendant as `CallContext::KotlinUse`
   (`extraction/src/cfg_builder.rs:705-713`), while the helper only checks for
   a lambda, not for a `.use` callee (`extraction/src/cfg_builder.rs:406-427`).
   `scope_exit` then also treats any alloc with a `KotlinUse` node within three
   normal CFG hops as context-managed (`analysis/src/scope_exit.rs:75-87`,
   `analysis/src/scope_exit.rs:114-139`). This can turn ordinary Kotlin
   trailing-lambda calls such as `map {}`, `let {}`, `also {}`, or `run {}` into
   resource boundaries. Current fixture coverage only has the positive
   `file.bufferedReader().use { ... }` case
   (`atlas-cli/tests/fixtures/kotlin/use_resource.kt:3-8`), so the false-positive
   path is untested.

5. **[P1] Ruby block resource classification is overbroad in both Full and Lazy
   paths.** The CFG builder marks any Ruby `call` with a `block` or `do_block`
   child as `CallContext::RubyBlock`
   (`extraction/src/cfg_builder.rs:671-679`), and `has_block_child` only checks
   child node kinds (`extraction/src/cfg_builder.rs:398-404`). This is correct
   for `File.open { ... }`, but also catches non-resource blocks such as
   `items.each { ... }` or `transaction { ... }`. Current fixture coverage only
   has the positive `File.open` case
   (`atlas-cli/tests/fixtures/ruby/block_resource.rb:1-5`).

6. **[P1] Scope-exit "returned or escaped resource" protection is not proven
   on the real compose path.** `scope_exit` only suppresses auto-free when it
   sees `SemanticEffectKind::Return` or `Escape` carrying
   `ValueSource::Local` (`analysis/src/scope_exit.rs:42-56`,
   `analysis/src/scope_exit.rs:92-104`). The current composer creates
   `Escape` from `ValueSource::CallReturn` for callee-level escape rules
   (`analysis/src/effect_composer.rs:259-271`), and the search for
   `SemanticEffectKind::Return` shows no source-to-effect synthesis in the
   normal extraction/composition path. That means the intended guard may not
   protect real code that returns a local resource, even though unit tests can
   manually construct such effects.

7. **[P2] `eligible_for_implicit_cleanup` documentation and behavior diverge.**
   `scope_exit` documents `None` as backward-compatible eligible
   (`analysis/src/scope_exit.rs:10-14`), but the implementation uses
   `unwrap_or(false)` (`analysis/src/scope_exit.rs:89-90`). New effects from
   `EffectComposer` set `Some(eligible)` (`analysis/src/effect_composer.rs:230-236`),
   so fresh paths may work, but older/manual effects and the documented contract
   disagree.

8. **[P2] Lazy callsite remap is functionally improved but still silently hides
   store failures.** Lazy now remaps provisional byte-based callsite ids against
   structural DB callsites (`lazy/src/loader.rs:160-176`) before writing unit
   data and updating callsite args (`lazy/src/loader.rs:197-224`). That fixes
   the earlier P0 design gap. However, `find_callsites_by_file` is followed by
   `unwrap_or_default()` (`lazy/src/loader.rs:166-169`), so a store error is
   indistinguishable from "no callsites". For a lazy path whose correctness
   depends on DB structural callsites, this should be propagated or surfaced as
   a diagnostic.

9. **[P2] Lazy frontend coverage is fixed for ArkTS/Cangjie, but the registry is
   still duplicated.** `get_cached_frontend` now includes ArkTS and Cangjie
   (`lazy/src/loader.rs:491-508`). That resolves the immediate missing-language
   path. The list remains hard-coded separately from the compiled-language list
   in `LanguageCapabilityProfile::all_compiled`
   (`types/src/capability.rs:366-400`), so future language additions can drift
   again.

10. **[P2] `sync --analysis manifest` still rebuilds summaries after a
    manifest-only reindex.** The sync engine now skips resolution/graph when
    references are not produced (`filesync/src/sync_engine.rs:155-186`), but the
    CLI command then loops changed files and calls
    `SummaryStore::build_for_function` regardless of analysis mode
    (`atlas-cli/src/commands/sync.rs:109-145`). `SummaryBuilder` reads existing
    data nodes by function and falls back to file-level nodes
    (`analysis/src/summary.rs:40-87`), so a manifest sync can rebuild summaries
    from stale pre-existing dataflow or report confusing skipped/empty counts.

### Language-by-Language Full/Lazy Status

1. TypeScript/JavaScript/ArkTS:
   Full and Lazy share the same dataflow/CFG extractor; ArkTS lazy entry is now
   present in the cache. React cleanup scoping is better than the old pure
   function-wide path when CFG annotates `ReactEffectCleanup`, but the fallback
   still marks every Free in the function deferred when any cleanup return
   exists (`analysis/src/effect_composer.rs:303-325`). File-level CFG capability
   overreporting still affects ArkTS if CFG remains unsupported in its profile.

2. Python:
   Full and Lazy share CFG and resource composition. Plain Python has
   `implicit_scope_cleanup=false` now (`analysis/src/resource_ops.rs:285`), so
   context-managed cleanup should depend on `PythonWith`. The remaining risk is
   the generic `scope_exit` returned/escaped-local guard not being proven by the
   real composer path.

3. Java:
   Full and Lazy share CFG and try-with-resources handling. The same
   `scope_exit` guard limitation applies. Constructor/callee normalization still
   needs source-to-effect negative tests, but this pass did not find a new Java
   entry-point split.

4. C:
   Full and Lazy share dataflow. C now has `implicit_scope_cleanup=false`
   (`analysis/src/resource_ops.rs:121-154`), which is the right direction for
   raw C allocation APIs. CFG/dataflow paths should verify explicit `free` and
   returned-resource cases, because `scope_exit` should not hide leaks here.

5. C++:
   C API producers in the C++ config are explicitly marked
   `implicit_cleanup=false` (`analysis/src/resource_ops.rs:157-186`), while the
   language default remains RAII cleanup (`analysis/src/resource_ops.rs:196-202`).
   This is materially improved, but producer patterns not listed in the C API
   set default back to implicit cleanup through
   `eligible_for_implicit_cleanup` (`analysis/src/resource_ops.rs:611-622`).

6. Go:
   Full and Lazy share dataflow/CFG. `defer` consumption still depends on
   precise CFG `CallContext::GoDefer` and callee normalization. No separate TUI
   lazy path exists.

7. C#:
   Full and Lazy share CFG and `CSharpUsing`. The resource config expects
   canonical constructor patterns like `new FileStream`
   (`analysis/src/resource_ops.rs:383-405`), so fixture coverage must verify
   source parse -> callee normalization -> effect matching, not only rules.

8. Rust:
   Full and Lazy share CFG and implicit Drop. `std::mem::forget` is modeled as a
   consumer and escape pattern (`analysis/src/resource_ops.rs:363-379`), but the
   returned/escaped-local guard still depends on effects the composer may not
   synthesize for ordinary `return local` source.

9. PHP:
   Full and Lazy dataflow are available, but CFG is unsupported in the language
   profile (`types/src/capability.rs:1181-1194`). Lazy unit masks now gate CFG,
   but Full file-level `CapabilityMask::from_layers("dataflow")` still
   advertises CFG unless file-level mask construction is changed.

10. Ruby:
    Ruby now advertises CFG support (`types/src/capability.rs:1244-1257`) and
    has a CFG block path, but block classification is too broad because every
    block call is treated as `RubyBlock`. Add negative block fixtures before
    treating the resource lifecycle path as stable.

11. Kotlin:
    Kotlin lazy and full paths now enter the same CFG/dataflow code, and the
    positive `.use` fixture exists. The classification is too broad because it
    checks only "has lambda" and nearby `KotlinUse`, not callee identity.

12. Cangjie:
    Full/Lazy can enter the language now, and CFG extraction uses the typed
    matrix. The metadata list still omits `"cfg"`, so all-language capability
    tests should catch it.

### Verification Still Needed

1. Run an all-language capability consistency test after fixing Cangjie metadata.
2. Add negative semantic fixtures for Kotlin trailing lambdas that are not
   `.use`, and Ruby block calls that are not resource producers.
3. Add source-level returned-resource fixtures that prove `return local` creates
   the effects `scope_exit` needs to suppress auto-free.
4. Add MCP `trace_variable` tests that assert top-level `lazy_diagnostics` and
   `analysis_contract` include dataflow-layer state after lazy dataflow runs.
5. Add Full-mode file-state tests for CFG-unsupported languages such as PHP to
   ensure file-level masks do not claim CFG just because the layer is
   `"dataflow"`.

## 2026-06-03 Review Pass 6: Post-Implementation Re-Audit

Scope: re-review after the latest implementation pass that addressed Review
Pass 5. This pass covers CLI index/sync, MCP trace/search/context/semantic
tools, TUI, Full and Lazy dataflow paths, and all compiled language profiles.

Verification run during this pass:

```text
cargo test -p analysis test_context_managed_always_eligible --quiet
```

Result: passed. The run emitted non-fatal warnings, including
`unused_mut` in `types/src/capability.rs:369` under the current feature set and
an unused local in a scope-exit test.

### Fixes Confirmed Since Pass 5

1. **File-level dataflow no longer implies CFG by layer name alone.**
   `CapabilityMask::from_layers("dataflow")` now sets MANIFEST, STRUCTURAL,
   CALL_EDGES, and DATAFLOW, but not CFG
   (`types/src/structs.rs:758-764`). `write_file_facts` adds CFG only when
   `facts.cfg_nodes` is non-empty (`db/src/store_writers.rs:720-723`). This
   addresses the previous Full-mode PHP/ArkTS file-level CFG overreporting
   concern.

2. **Cangjie capability metadata now derives from the typed matrix.**
   Cangjie builds `supported_features` and `unsupported_features` from
   `FeatureMatrix` (`types/src/capability.rs:913-970`), so the CFG matrix/list
   consistency test should no longer fail for Cangjie
   (`types/src/capability.rs:1684-1708`).

3. **Ruby block classification is narrowed to resource block calls.**
   Ruby now requires both a block child and `is_ruby_resource_block_call`
   (`extraction/src/cfg_builder.rs:714-718`). The predicate checks known
   resource block callees such as `File.open`, `IO.open`, `Dir.chdir`, and
   socket constructors (`extraction/src/cfg_builder.rs:449-470`).

4. **Kotlin `.use {}` classification is narrowed by callee text.**
   Kotlin now requires `has_lambda_child` and `is_kotlin_use_call`
   (`extraction/src/cfg_builder.rs:749-753`). The helper skips `call_suffix`
   nodes and requires a non-suffix child text ending in `.use`
   (`extraction/src/cfg_builder.rs:430-447`).

5. **`sync --analysis manifest` no longer rebuilds summaries.**
   The sync command computes `has_dataflow = mode.produces_dataflow()` and
   only runs the changed-file summary rebuild when true
   (`atlas-cli/src/commands/sync.rs:12-18`,
   `atlas-cli/src/commands/sync.rs:110-148`).

6. **Lazy unit capability masks are now CFG-gated.**
   Lazy writes DATAFLOW unconditionally for successful unit dataflow, but sets
   CFG only when the language profile supports CFG and actual CFG nodes exist
   (`lazy/src/loader.rs:187-195`, `lazy/src/loader.rs:235-261`). The prebuilt
   cache path uses the same profile/count gate (`lazy/src/loader.rs:303-352`).

7. **Lazy frontend cache includes ArkTS and Cangjie.**
   `get_cached_frontend` now includes both languages
   (`lazy/src/loader.rs:491-508`), so the immediate lazy entry gap is closed.

### Remaining Findings

1. **[P1] MCP dataflow diagnostics still do not produce a correct combined
   `analysis_contract`.** `handle_trace_variable` now converts an Engine
   `LazySummary` into `diag.dataflow` (`atlas-mcp/src/tools/trace.rs:260-270`),
   but it only creates `combined_lazy_diag` from `structural_lo`
   (`atlas-mcp/src/tools/trace.rs:253-258`). If structural lazy extraction did
   not run because the file was already structural, there is no
   `lazy_diagnostics` block even though Engine may have triggered lazy
   dataflow. If structural did run, `diag.analysis_contract` still comes from
   `LazyDiagnostics::from_structural`, so its capability mask is not recomputed
   after the dataflow summary is added (`atlas-mcp/src/tools/trace.rs:294-297`,
   `lazy_response.rs:238-257`). This affects MCP `trace_variable` on the common
   manifest/structural + lazy-dataflow path.

2. **[P1] `LazyWindow.capability_mask` remains default in the service return
   path, so MCP semantic tools can underreport dataflow/CFG capability.**
   The planner initializes `LazyWindow.capability_mask` to default
   (`lazy/src/planner.rs:155-166`, `lazy/src/planner.rs:234-245`), and
   `LazyDataflowService::ensure_for_position` only fills counts, pending jobs,
   and precision tier (`lazy/src/lib.rs:51-81`). `LazyDiagnostics::from_layers`
   merges `dw.capability_mask` into `analysis_contract`
   (`lazy_response.rs:238-247`). Tools that pass a real `LazyWindow`, such as
   branch diff and lifecycle, therefore show dataflow-layer stats but a
   capability contract with no dataflow/CFG bits
   (`atlas-mcp/src/tools/branch_diff.rs:86-94`,
   `atlas-mcp/src/tools/lifecycle.rs:100-108`).

3. **[P1] Engine drops `LazySummary` when trace slicing returns no path.**
   `Engine::trace_variable` always runs `ensure_for_position` first, but only
   attaches the resulting `lazy_summary` inside `if let Some(ref mut path) =
   resp.result` (`atlas-engine/src/lib.rs:437-443`). If the locator or slicer
   returns a partial response with no `TracePath`, MCP cannot recover the
   dataflow summary and cannot build truthful lazy diagnostics. This is a lazy
   path observability bug, not an extraction bug.

4. **[P2] Lazy callsite remap still soft-fails on store errors.** The P0
   remap design is fixed: lazy data nodes are remapped against structural DB
   callsites before `update_callsite_arg_data_nodes`
   (`lazy/src/loader.rs:157-224`). However, `find_callsites_by_file` errors are
   logged and replaced with an empty vector (`lazy/src/loader.rs:166-176`).
   That prevents crashes, but it also silently degrades the core join that
   summaries and trace bridges depend on. At minimum, this should surface in
   `LazyWindow`/MCP diagnostics.

5. **[P2] Kotlin/Ruby overbroad classifications are narrowed, but negative
   coverage is still absent.** Current integration tests assert the positive
   `KotlinUse` and `RubyBlock` cases
   (`atlas-cli/tests/integration.rs:2453-2461`,
   `atlas-cli/tests/integration.rs:2592-2600`). There is no fixture proving
   `list.map {}`, `run {}`, `users.each {}`, or `transaction {}` do not create
   `KotlinUse`/`RubyBlock`/`BlockExit`. Given how syntax-text based the new
   filters are, negative tests are required before treating the lifecycle
   boundary classification as stable.

6. **[P2] Returned-resource suppression is still only proven with hand-built
   effects.** `scope_exit` suppresses auto-free only when `Return` or `Escape`
   effects carry `ValueSource::Local` (`analysis/src/scope_exit.rs:42-56`,
   `analysis/src/scope_exit.rs:92-104`). The new tests manually construct a
   `Return` effect, but the source parse -> dataflow -> compose path still does
   not visibly synthesize `SemanticEffectKind::Return` for ordinary
   `return local` statements. This affects Rust/C++/Java/C#/Python context
   paths where ownership should transfer to the caller.

7. **[P2] CLI `atlas index` still builds summaries in default structural mode.**
   CLI index returns early only for manifest (`atlas-cli/src/commands/index.rs:312-325`).
   Structural mode continues through resolution, graph building, and
   `phase_build_summaries` (`atlas-cli/src/commands/index.rs:366-372`) even
   though structural extraction does not produce dataflow. The per-file cleanup
   before write removes stale facts for dirty files
   (`atlas-cli/src/commands/index.rs:267-274`), so this is less severe than the
   sync bug that was fixed. It is still a mismatched path: `sync` now gates
   summaries on `produces_dataflow()`, while `index` does not.

8. **[P3] Lazy frontend language registration is still duplicated.** ArkTS and
   Cangjie are now included, but `get_cached_frontend` remains a hard-coded
   list (`lazy/src/loader.rs:491-508`) separate from
   `LanguageCapabilityProfile::all_compiled` (`types/src/capability.rs:366-400`).
   Future languages can still drift between non-lazy and lazy entry points.

9. **[P3] Some integration tests leave diagnostic `eprintln!` output in normal
   runs.** The Kotlin resource integration test prints all CFG nodes,
   DataNodes, and Alloc effects (`atlas-cli/tests/integration.rs:2463-2485`).
   This is useful while debugging, but it makes normal test output noisy.

### Path Coverage Summary

1. **CLI index**
   Manifest path is explicitly early-returned. Full path reaches dataflow/CFG
   and summary build. Structural path reaches summary build despite not
   producing dataflow; this remains the main CLI-index mismatch.

2. **CLI sync**
   Manifest now skips resolution/graph in `SyncEngine` and skips summary
   rebuild in the CLI wrapper. Full still rebuilds summaries. Structural sync
   skips summaries because `produces_dataflow()` is false.

3. **MCP**
   Search/context/caller/forward traces remain structural/call-graph oriented.
   `trace_variable` correctly routes through high-level `Engine`, but
   top-level lazy diagnostics and contracts are still not fully combined.
   Lifecycle/branch-diff tools still rely on `LazyWindow.capability_mask`, which
   is not filled.

4. **TUI**
   TUI still uses `RawTraceEngine::trace_callers`
   (`atlas-cli/src/tui/app.rs:348-352`). It does not expose variable/dataflow
   trace, so lazy dataflow is not a TUI path today.

### Language Status After This Pass

1. TypeScript/JavaScript/ArkTS:
   Full and Lazy share dataflow/CFG implementation. ArkTS lazy entry exists.
   MCP contract reporting remains the primary user-visible gap after lazy
   dataflow.

2. Python:
   Plain implicit cleanup is disabled; context cleanup depends on CFG
   `PythonWith`. Returned-resource handling still needs source-level proof.

3. Java:
   Full/Lazy CFG and try-with-resources share the same scope-exit path.
   Returned-resource source-level proof is still missing.

4. C:
   Manual cleanup default is correct. Full/Lazy tests should focus on explicit
   `free` and no false scope-exit cleanup.

5. C++:
   C API producer patterns are marked ineligible for implicit cleanup, while
   the language default remains RAII. Unlisted producer patterns default to
   implicit cleanup, so source-level negative coverage is still important.

6. Go:
   Full/Lazy share dataflow/CFG. No new Go-specific path regression found in
   this pass.

7. C#:
   Full/Lazy share `CSharpUsing`. Constructor/resource normalization still
   needs end-to-end fixture coverage beyond matcher-level confidence.

8. Rust:
   Full/Lazy share CFG and implicit Drop. `return local`/`mem::forget` behavior
   should be verified from real source, not only constructed effects.

9. PHP:
   CFG is unsupported in the profile, and file/unit masks now avoid claiming
   CFG unless nodes exist. Lazy dataflow remains available.

10. Ruby:
    Resource block classification is narrowed, but negative block fixtures are
    still missing.

11. Kotlin:
    `.use` classification is narrowed, but negative trailing-lambda fixtures are
    still missing. The bounded successor walk in `scope_exit` also still relies
    on nearby CFG shape rather than a resource-chain identity.

12. Cangjie:
    Lazy entry and capability metadata are now aligned with the typed matrix.
    No Cangjie-specific runtime regression found in this pass.

### Recommended Next Verification

1. Add MCP `trace_variable` tests where structural is already complete and lazy
   dataflow runs; assert `lazy_diagnostics.dataflow` and
   `analysis_contract.safe_conclusions` reflect DATAFLOW/CFG.
2. Populate `LazyWindow.capability_mask` from unit extraction state after
   `LazyDataflowLoader::ensure`, then assert branch diff/lifecycle diagnostics
   include dataflow/CFG capability.
3. Add negative Kotlin and Ruby lifecycle fixtures.
4. Gate CLI index summary build on `mode.produces_dataflow()` or document why
   structural summary build is intentional.
5. Add source-level returned-resource tests for at least Rust, C++, Python
   context manager, Java try-with-resources, and C# using.

## 2026-06-04 Review Pass 7: External Pre-release Report Validation

Scope: validate the pasted "Atlas v1.3.1 Comprehensive Pre-release Review
Report" against the current repository. This is not a new full audit; it checks
whether that report's concrete claims are true, false, or overstated.

### Validation Verdict

The pasted report is **partially true, but not fully reliable as written**.
Several severe MCP issues are real. Some documentation claims are real but
overstated. The CFG panic claim is not supported by the current code path.

### Confirmed True / Actionable

1. **MCP `wait_for_task` blocks the current-thread runtime.**
   `McpServer::serve` uses `tokio::runtime::Builder::new_current_thread()`
   (`atlas-mcp/src/lib.rs:50-54`). `wait_for_task` calls
   `std::thread::sleep` inside a polling loop
   (`atlas-mcp/src/tools/wait_for.rs:93-106`). Tool dispatch also holds the
   router mutex while calling `router.call_tool`
   (`atlas-mcp/src/lib.rs:219-249`), so `wait_for_task` blocks the runtime and
   keeps the router unavailable for other tools until it returns.

2. **The MCP `symbol` tool has a real `qname` vs `symbol` inconsistency.**
   Current `crates/atlas-mcp/README.md` documents `symbol` as the required
   argument for the `symbol` tool (`README.md:50-54`), but the actual schema
   requires `qname` (`atlas-mcp/src/tools/mod.rs:1112-1128`) and the handler
   returns `Missing required 'qname' parameter`
   (`atlas-mcp/src/tools/mod.rs:1448-1453`). The report's "6+ docs" scope was
   not confirmed, but the primary MCP README mismatch is real.

3. **Poisoned MCP router mutex is not recovered.**
   `lock_router` maps a poisoned lock to a permanent internal error instead of
   recovering with `PoisonError::into_inner`
   (`atlas-mcp/src/lib.rs:87-91`). The direct progress setup/cleanup locks use
   the same non-recovering pattern (`atlas-mcp/src/lib.rs:185-189`,
   `atlas-mcp/src/lib.rs:251-257`).

4. **Progress sender cleanup is not panic-safe.**
   `progress_sender` is installed before dispatch
   (`atlas-mcp/src/lib.rs:175-190`) and cleared only after dispatch returns
   (`atlas-mcp/src/lib.rs:251-257`). If dispatch panics, cleanup is skipped.
   In practice this is coupled with the mutex-poison issue, so these should be
   fixed together with a guard/catch-unwind strategy.

5. **DB silent error swallowing exists, but the report's count/severity needs
   triage.**
   Confirmed examples include dropping semantic-effect serialization errors
   (`db/src/store_writers.rs:523-527`), silently skipping malformed callsite
   rows and defaulting malformed `args_json`
   (`db/src/store/unit_extraction_state.rs:297-305`), and treating extraction
   state query errors as "not found" (`db/src/store/file_extraction_state.rs:18-27`).
   Some `.ok()` instances are compatibility paths, so this should be triaged by
   data-loss risk rather than fixed mechanically.

6. **MCP README `domain_rules` row is stale.**
   The README lists `language`, `kind`, `patterns`, `status`
   (`crates/atlas-mcp/README.md:63`), while the schema documents
   `rule_kind`, `pattern`, `rule_id`, `source`, `confidence`, and
   `min_confidence` (`atlas-mcp/src/tools/mod.rs:1325-1344`).

7. **Several CLI/documentation mismatch claims are real.**
   The CLI enum only exposes `status`, `doctor`, `index`, `sync`, `files`, and
   `mcp` (`atlas-cli/src/lib.rs:68-119`), while `docs/trace-contract.md`
   still documents `atlas trace ...` commands (`docs/trace-contract.md:326-329`).
   `doctor` still recommends non-existent `atlas init`
   (`atlas-cli/src/commands/doctor.rs:78-82`). TUI auto-index still calls
   `index::run(".", ...)` instead of using the function's project root
   (`atlas-cli/src/main.rs:83-86`). The Chinese exit prompt also remains
   (`atlas-cli/src/tui/app.rs:739`).

8. **MCP index include/exclude arrays are uncapped.**
   `handle_index` parses `include` and `exclude` arrays without an entry-count
   cap (`atlas-mcp/src/tools/index.rs:60-76`). `include_roots` has a dedicated
   cap elsewhere, so this is a real consistency gap.

9. **Summary `build_all` still calls a builder closure while holding a write
   transaction/connection lock.**
   `SummaryStore::build_all` locks and opens a transaction before invoking
   `build_fn(store, &sym.id)` (`db/src/store/summary.rs:136-147`). Production
   callers pass `SummaryBuilder::build`, which reads from the store. This
   remains a credible deadlock/reentrancy risk unless Store lock behavior makes
   that safe by construction.

### False, Outdated, Or Overstated

1. **CFG `walk_if` index panic is outdated.**
   Current code guards both branch-edge mutations with
   `if self.edges.len() > saved_edge_count`
   (`extraction/src/cfg_builder.rs:981-1003`). The report describes the old
   unguarded indexing behavior.

2. **"Every documentation file says symbol" is overstated.**
   The MCP README mismatch is real, but a broad search did not confirm the
   report's "all 6+ docs" wording. Treat the issue as a concrete MCP README /
   schema inconsistency, not as a verified six-document drift.

3. **`rebuild_in_progress` "can stick forever" needs narrower wording.**
   On rebuild failure the code calls `mark_rebuild_finished` and reschedules
   (`atlas-mcp/src/tools/mod.rs:482-485`). On success it intentionally keeps
   `rebuild_in_progress` true until a later request applies the pending graph
   (`atlas-mcp/src/tools/mod.rs:473-481`, `430-460`). That may be an
   observability/latency concern, but it is not clearly a stuck-state bug.

### Recommended Priority From This Validation

1. Fix MCP runtime blocking: make `wait_for_task` async/non-blocking or move MCP
   service to a multi-thread runtime, and avoid sleeping while holding the
   router lock.
2. Fix MCP panic/poison cleanup: recover or replace the poisoned router policy,
   and clear `progress_sender` with a guard.
3. Align `symbol` schema/docs. Prefer accepting both `qname` and `symbol` for
   backward compatibility, then document one canonical name.
4. Update stale docs: `domain_rules`, non-existent CLI commands, `atlas init`,
   old trace tool names, and old `atlas_resume`/`atlas_jobs` names.
5. Triage DB silent swallowing by data-loss risk, starting with semantic
   effects, callsite args, extraction state, and summary build errors.
6. Do not treat the CFG panic claim as a current release blocker unless new
   evidence appears.

## 2026-06-04 Review Pass 8: Post-Fix Scope-Correct Validation

Scope: re-check the user's latest fixes for Atlas project code only. This pass
updates the Pass 7 findings after the implementation changes and re-checks
CLI, MCP, TUI, lazy/non-lazy, and language-facing paths.

### Validation Verdict

The latest fixes close several previously confirmed issues, especially MCP
`wait_for_task`, `symbol` schema, index pattern caps, Cangjie/lazy entry
coverage, and the testing/architecture path-coverage rule. Remaining issues are
mostly MCP response-contract drift, stale docs/UI text, and a narrowed summary
locking risk.

### Confirmed Fixed

1. **MCP `wait_for_task` no longer blocks the current-thread runtime.**
   `handle_wait_for_task` is now async and uses `tokio::time::sleep`
   (`crates/atlas-mcp/src/tools/wait_for.rs:31-60`,
   `crates/atlas-mcp/src/tools/wait_for.rs:136-142`). The server special-cases
   `wait_for_task`, clones `task_manager`, releases the router mutex, awaits the
   poll loop, and only re-locks for project activation
   (`crates/atlas-mcp/src/lib.rs:226-271`).

2. **Actual MCP `symbol` schema and handler now agree on `symbol`.**
   The tool schema requires `symbol`
   (`crates/atlas-mcp/src/tools/mod.rs:1114-1131`), and the handler reads
   `get_str(args, "symbol")` with a matching missing-argument error
   (`crates/atlas-mcp/src/tools/mod.rs:1451-1456`).

3. **MCP `index` include/exclude pattern caps are present in both paths.**
   Foreground caps are checked at
   `crates/atlas-mcp/src/tools/index.rs:81-98`; background caps are checked at
   `crates/atlas-mcp/src/tools/index.rs:209-226`.

4. **Doctor and no-subcommand TUI auto-index were corrected.**
   `doctor` now recommends `atlas index`
   (`crates/atlas-cli/src/commands/doctor.rs:78-82`). TUI auto-index passes the
   function's `project_root` through to `index::run`
   (`crates/atlas-cli/src/main.rs:73-90`), instead of hard-coding `"."`.

5. **Cangjie capability metadata and lazy frontend entry are aligned.**
   Cangjie now derives `supported_features` / `unsupported_features` from the
   typed `FeatureMatrix`, including CFG
   (`crates/atlas-engine/crates/types/src/capability.rs:913-970`). Lazy
   frontend cache includes ArkTS and Cangjie plus the other compiled languages
   (`crates/atlas-engine/crates/lazy/src/loader.rs:506-520`).

6. **Lazy callsite remap and unit capability mask semantics improved.**
   Lazy data nodes are remapped from provisional byte callsite IDs to structural
   DB callsite IDs before writing
   (`crates/atlas-engine/crates/lazy/src/loader.rs:157-224`). Unit-level
   prebuilt and write paths gate CFG on both language support and actual CFG
   nodes (`crates/atlas-engine/crates/lazy/src/loader.rs:235-270`,
   `crates/atlas-engine/crates/lazy/src/loader.rs:340-360`).

7. **The "verify every affected path" rule is now in base docs.**
   `docs/testing.md` explicitly requires all affected analysis/user-entry paths
   to be listed and verified, including lazy and non-lazy, file/unit state, and
   database plus user-visible output (`docs/testing.md:7-35`). The same rule is
   reflected in architecture principles (`docs/architecture.md:10-12`,
   `docs/architecture.md:248-253`).

### Remaining Findings

1. **[P1] MCP `trace_variable` still loses lazy dataflow diagnostics on the
   common structural-cache-hit path.**
   `Engine::trace_variable` always triggers lazy dataflow and stores the summary
   on the response envelope (`crates/atlas-engine/src/lib.rs:386-442`), but the
   MCP layer only creates `combined_lazy_diag` from `structural_lo`
   (`crates/atlas-mcp/src/tools/trace.rs:253-258`). If structural lazy did not
   run because the file was already structural, there is no top-level
   `lazy_diagnostics` or `analysis_contract` even though dataflow was built or
   checked. In addition, the merge reads `path.lazy_summary`
   (`crates/atlas-mcp/src/tools/trace.rs:265-269`), while the high-level Engine
   writes `resp.lazy_summary` on the envelope. Result: MCP can still underreport
   lazy dataflow state after a successful variable trace.

2. **[P2] `LazyWindow.capability_mask` now reports DATAFLOW, but still
   underreports CFG for CFG-backed semantic MCP tools.**
   `LazyDataflowService` deliberately sets MANIFEST/STRUCTURAL/CALL_EDGES/
   DATAFLOW and omits CFG from the returned window mask
   (`crates/atlas-engine/crates/lazy/src/lib.rs:81-92`,
   `crates/atlas-engine/crates/lazy/src/lib.rs:126-138`). This is conservative,
   but `LazyDiagnostics::from_layers` builds the MCP `analysis_contract` by
   merging that window mask. `branch_diff` and `lifecycle` can actually load CFG
   via lazy dataflow, so their diagnostics may still say CFG is unavailable or
   omit CFG-backed safe conclusions.

3. **[P2] MCP router poison recovery is incomplete in progress setup.**
   `lock_router` now recovers with `PoisonError::into_inner`, and
   `ProgressGuard` clears `progress_sender` on drop
   (`crates/atlas-mcp/src/lib.rs:87-88`,
   `crates/atlas-mcp/src/lib.rs:217-223`). But progress setup still directly
   calls `self.router.lock().map_err(...)`
   (`crates/atlas-mcp/src/lib.rs:182-188`). A poisoned mutex before progress
   setup still fails progress-enabled `index` / `project` / `search` calls
   despite the new recovery policy.

4. **[P2] MCP README still documents the wrong required parameter for
   `symbol`.**
   Code/schema now require `symbol`, but the README table still says
   `qname` (`crates/atlas-mcp/README.md:49-53`). This is now a docs-only drift,
   reversed from the earlier code/schema bug.

5. **[P2] Trace contract docs still advertise a non-existent CLI trace
   command.**
   `docs/trace-contract.md` still shows `atlas trace --kind ...`
   (`docs/trace-contract.md:15-18`, `docs/trace-contract.md:317-322`), while
   the current CLI command set does not expose a `trace` subcommand.

6. **[P3] TUI exit confirmation remains Chinese-only.**
   `render_exit_confirmation` still renders `再次按esc确认退出`
   (`crates/atlas-cli/src/tui/app.rs:735-742`). If the CLI/TUI UX target is
   English-only, this previous finding remains unresolved.

7. **[P3] Summary `build_all` locking risk is narrower, but not eliminated.**
   `SummaryStore::build_all` still opens a write transaction before calling
   `build_fn(store, &sym.id)` (`crates/atlas-engine/crates/db/src/store/summary.rs:112-140`).
   The store now has a dedicated read connection for file-backed DBs
   (`crates/atlas-engine/crates/db/src/store/mod.rs:57-85`), so the broad
   "production full index deadlocks" wording should be downgraded. However,
   in-memory stores still fall back to the write connection; any summary builder
   closure that performs store reads in that mode remains reentrant-lock risky.

### Recommended Next Verification

1. Add MCP `trace(kind="variable")` tests for both structural-cache-hit and
   structural-lazy-built cases; assert `lazy_diagnostics.dataflow` and
   top-level `analysis_contract` are present and reflect `resp.lazy_summary`.
2. Add MCP `branch_diff` / `lifecycle` tests where lazy CFG is built, then
   assert the returned `analysis_contract` does not underreport CFG-backed
   conclusions.
3. Align `crates/atlas-mcp/README.md` and `docs/trace-contract.md` with the
   actual public tool/CLI surface.
4. Decide whether TUI text should be localized or English-only; then test the
   user-facing prompt.
5. If summary builders must support in-memory stores, add a regression test
   where `build_all` closure performs a real store read.

## 2026-06-04 Review Pass 9: Post-Pass-8 Fix Recheck

Scope: re-check the fixes made after Pass 8, focusing on the exact remaining
findings from that pass: MCP trace lazy diagnostics, lazy CFG capability
contract, progress mutex recovery, MCP/trace docs, TUI prompt, and summary
locking.

### Confirmed Fixed

1. **MCP README `symbol` row is now aligned.**
   The `symbol` tool table now lists `symbol` as the required argument and keeps
   `include_roots` / `limit` in optional arguments
   (`crates/atlas-mcp/README.md:49-56`).

2. **Progress setup now recovers poisoned router locks consistently.**
   `lock_router` recovers with `PoisonError::into_inner`
   (`crates/atlas-mcp/src/lib.rs:87-88`), progress setup now uses the same
   recovery behavior (`crates/atlas-mcp/src/lib.rs:182-185`), and
   `ProgressGuard` still clears `progress_sender` on drop
   (`crates/atlas-mcp/src/lib.rs:215-220`).

3. **TUI exit confirmation is now English.**
   `render_exit_confirmation` renders `Press ESC again to confirm exit`
   (`crates/atlas-cli/src/tui/app.rs:735-742`).

4. **Trace usage examples were mostly corrected.**
   The old CLI examples were removed from the usage section, and the document
   now states that trace is MCP-only (`docs/trace-contract.md:313-318`).

### Remaining Findings

1. **[P1] MCP `trace_variable` still does not actually merge Engine lazy
   dataflow summary.**
   The attempted fix still reads `path.lazy_summary`
   (`crates/atlas-mcp/src/tools/trace.rs:269-281`). But the slicer initializes
   `TracePath.lazy_summary` to `None`
   (`crates/atlas-engine/crates/analysis/src/trace/slicer.rs:264-275`), while
   the high-level Engine writes the real lazy metadata to the response envelope
   as `resp.lazy_summary` (`crates/atlas-engine/src/lib.rs:438-441`). Therefore
   the new `from_dataflow_summary` branch will not run on the common path, and
   top-level `lazy_diagnostics` / `analysis_contract` can still be absent after
   lazy dataflow runs.

2. **[P1] `from_dataflow_summary` builds an internally contradictory
   analysis contract.**
   Even if the branch above were reached, `from_dataflow_summary` stores
   `dataflow: Some(...)` but constructs `analysis_contract` from
   `CapabilityMask::default()` (`crates/atlas-mcp/src/tools/lazy_response.rs:272-290`).
   That means the response can report dataflow layer stats while
   `analysis_contract.safe_conclusions` still says dataflow is unavailable.
   The contract needs a DATAFLOW-capable mask at minimum, and should include
   CFG only when the actual lazy/window capability justifies it.

3. **[P1] `LazyWindow.capability_mask` now overreports CFG.**
   Pass 8's underreporting issue was changed by setting CFG unconditionally
   whenever any lazy unit is built or cached
   (`crates/atlas-engine/crates/lazy/src/lib.rs:81-95`,
   `crates/atlas-engine/crates/lazy/src/lib.rs:128-141`). That is too broad:
   ArkTS and PHP still explicitly mark CFG unsupported
   (`crates/atlas-engine/crates/types/src/capability.rs:844-899`,
   `crates/atlas-engine/crates/types/src/capability.rs:1169-1217`), and a lazy
   window may have dataflow without actual CFG nodes. This can make
   `analysis_contract` claim branch-level control-flow safety for languages or
   windows where CFG is not available.

4. **[P2] `docs/trace-contract.md` still has a stale CLI entry in its top
   flow diagram.**
   The examples section now correctly says there is no `atlas trace`, but the
   diagram still lists `CLI: atlas trace --kind ...`
   (`docs/trace-contract.md:15-18`). This is a small but direct user-facing
   contradiction.

5. **[P3] Summary `build_all` in-memory reentrancy risk remains unchanged.**
   `build_all` still opens the write transaction before invoking
   `build_fn(store, &sym.id)` (`crates/atlas-engine/crates/db/src/store/summary.rs:112-141`).
   File-backed stores have a separate read connection, but in-memory stores
   still fall back to the write connection for reads
   (`crates/atlas-engine/crates/db/src/store/mod.rs:77-85`). `build_for_function`
   is safer because it computes the summary before opening its transaction
   (`crates/atlas-engine/crates/db/src/store/summary.rs:168-183`).

### Recommended Fix Direction

1. In MCP `handle_trace_variable`, read `resp.lazy_summary` from the envelope,
   not `resp.result.as_ref().and_then(|path| path.lazy_summary.as_ref())`.
2. Make `LazyDiagnostics::from_dataflow_summary` build its
   `analysis_contract` from a mask that includes DATAFLOW and any proven CFG
   capability, or pass the actual `LazyWindow` / capability mask instead of
   down-converting to `LazySummary`.
3. Rework `LazyDataflowService` window mask to aggregate persisted unit
   extraction-state masks, or at least gate CFG by language support and actual
   CFG node presence, mirroring `loader.rs`.
4. Remove the stale CLI branch from `docs/trace-contract.md`'s top diagram.
5. Add regression tests for MCP trace variable diagnostics in both structural
   cache-hit and structural-lazy-built paths; static helper tests are not enough
   for this contract.
