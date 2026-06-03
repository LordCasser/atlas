# TEMP Language Evolution Review

Status: temporary review notes, append-only during this audit.

Audit target: `docs/TEMP_LANGUAGE_EVOLUTION_PLAN.md`

Scope: review the current multi-language updates from the user's entry points
through language detection, grammar/frontend selection, query normalization,
CFG/dataflow extraction, semantic effect composition, storage/consumer exposure,
and per-language parsing depth.

Note: the current Codex session did not expose the configured `codegraph_*`
tools, so this pass uses local source inspection with `rg`, `sed`, and targeted
test/source reads.

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

