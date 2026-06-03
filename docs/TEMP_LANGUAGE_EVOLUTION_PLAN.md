# Atlas Multi-Language Evolution Plan

Status: temporary design note

This document defines how Atlas should evolve language support after the current
14-language `DataflowFull` milestone. It is intentionally concrete: each
language has target capabilities, implementation work, semantic analysis work,
test gates, and documentation updates.

## 1. Current Baseline

Atlas already has frontends, tree-sitter queries, fixtures, and capability
profiles for 14 languages:

- Default build: TypeScript, JavaScript, Python.
- `all-languages`: Java, C, C++, ArkTS, Go, C#, Rust, PHP, Ruby, Kotlin,
  Cangjie.

The current `DataflowFull` label should be treated as:

```text
DataflowFull = local_dataflow + use_def + call_arguments
             + returns_flow + interprocedural_summaries
```

It must not imply compiler-grade type resolution, complete CFG, complete
framework semantics, or complete language runtime modeling.

Known baseline risks:

- `DataflowFull` is too coarse for user expectations.
- CFG support is uneven and branch/loop traversal still needs hardening.
- Semantic analysis is strongest for C/C++; other languages mostly have generic
  resource-operation patterns, not language-specific consumers.
- Capability declarations, README tables, architecture tables, and tests can
  drift unless they are generated or checked together.

## 2. Capability Model

Keep `LanguageCapabilityProfile` as the user-facing truth, but make the
granularity more explicit. Each language should be evaluated by this conceptual
ladder (used for documentation and planning, **not** implemented as a new Rust
enum — the code authority is `FeatureMatrix` + `CapabilityMask`):

| Level | Required facts | Meaning |
|-------|----------------|---------|
| L0 Parse | parser + file metadata | File can enter indexing without panics. |
| L1 Structural | symbols, references, imports, scopes | Search, point lookup, file context. |
| L2 Call Graph | callsites + resolved internal calls | callers/callees/path/impact basics. |
| L3 Local Dataflow | bindings, data nodes, dataflow edges | variable origin inside a function. |
| L4 Interprocedural | function summaries, ArgToParam, ReturnToCall | bounded cross-function trace. |
| L5 CFG | entry/exit, statements, branches, loops, joins | branch-aware trace and impact. |
| L6 Semantic Effects | EffectIR from CFG + dataflow | alloc/free/store/nullify/escape effects. |
| L7 Language Semantics | per-language rule consumer | lifecycle, branch_diff, semantic impact. |

`CapabilityLevel::DataflowFull` can stay for compatibility, but docs and MCP
responses should also expose L0-L7 feature support explicitly through
`FeatureMatrix`, `CapabilityMask`, and diagnostics.

**Design Decision**: Do NOT introduce a parallel L0-L7 enum into the type system.
The existing `FeatureMatrix` (13 typed `FeatureSupport` fields) already provides
per-feature querying at finer granularity than an 8-level enum. L0-L5 map to
existing `FeatureMatrix` fields; L6-L7 are analysis-layer capabilities that
should use an independent `SemanticCapability` model, not be forced into the
extraction capability ladder. The L0-L7 table above serves as a conceptual
framework for documentation and planning only.

## 3. Architecture Direction

The language pipeline should remain layered:

```text
tree-sitter grammar
  -> per-language .scm queries
  -> LanguageFrontend / LanguageAdapter normalization
  -> ScopeTree + LexicalBinder
  -> DataFlowBuilder
  -> CfgBuilder
  -> SemanticBinder
  -> Store facts
  -> ReferenceResolver + SummaryBuilder + GraphBuilder
  -> analysis consumers
```

Semantic analysis should use the existing generic contract path:

```text
domain_rules::GenericRuleEngine
  -> per-language LanguageRuleKinds registry
  -> RuleMatch
  -> analysis consumer interprets matches
  -> ResourceOpConfig / OwnershipContract
  -> EffectComposer
  -> lifecycle / branch_diff / impact
```

Do not put language-specific ownership, resource, framework, or runtime strings
inside `domain_rules` core. The core matches rules; analysis consumers interpret
them.

### 3.1 Proposed Consumer Traits

Keep `OwnershipContract` as the lowest-level resource contract. Add narrowly
scoped consumers only when a language needs semantics beyond resource
allocation and release.

**Design Decision**: Do NOT introduce a parallel `LanguageSemanticConsumer` trait.
Instead, extend the existing `OwnershipContract` trait (already consumed by
`EffectComposer`) with language-specific classification methods. This avoids a
dual-trait maintenance burden and ensures all semantic effects flow through the
existing `EffectComposer → lifecycle/branch_diff/impact` pipeline.

```rust
// Extend existing OwnershipContract, do not create a parallel trait
pub trait OwnershipContract {
    // Existing methods:
    fn classify_return(&self, callee: &str) -> Vec<Classification>;
    fn classify_consumption(&self, callee: &str, arg_idx: usize) -> Vec<Classification>;

    // New methods to be added (per-language impls, not a separate trait):
    /// Language-specific statement-level boundary classification
    /// (e.g., Python `with`, Go `defer`, Kotlin `.use {}`, C# `using`)
    fn classify_boundary(
        &self,
        node_kind: &CfgNodeKind,
        callee: Option<&str>,
    ) -> Vec<SemanticEffect>;
}
```

Each language's semantic consumer (e.g., `CppOwnershipRules` in
`analysis/src/ownership_rules.rs`) should be an `impl OwnershipContract` for that
language. The existing generic `ResourceOpConfig` path (producers/consumers per
language) remains the primary mechanism for simple resource patterns; language
consumers are added only when a language needs boundary-specific classification
beyond callee-name matching.

**Prerequisites**: Before implementing any per-language `classify_boundary`,
the following gaps must be resolved:
- `EscapeTarget` in `types/src/effects.rs` needs a `Thread` variant (for Go
  goroutines, Kotlin coroutines) and an `AsyncContext` variant (for async
  boundaries).
- `BoundaryEffect` does not yet exist in the codebase; language boundary
  classification should produce `SemanticEffect` with existing
  `SemanticEffectKind` variants, not introduce a parallel effect type.

These traits and types live in `analysis`, not `types`, unless multiple lower
crates need them. Keep `types` limited to serializable effect records and
minimal shared contracts.

### 3.2 Domain Rule Registries

`domain_rules::kinds` currently has C/C++ registry support. Add one registry per
semantic family, not per incidental feature:

```text
domain_rules/src/kinds/
  c.rs
  rust.rs
  go.rs
  python.rs
  typescript.rs
  java.rs
  csharp.rs
  php.rs
  ruby.rs
  kotlin.rs
```

ArkTS may initially reuse TypeScript rules with `language='arkts'` overrides.
Cangjie should start minimal until language-specific conventions are stable.

**Note on EscapeTarget**: The current `EscapeTarget` enum in `types/src/effects.rs`
does not include `Thread` or `AsyncContext` variants. Languages that need
goroutine/coroutine escape semantics (Go §5.8, Kotlin §5.13) require extending
this enum. This extension must be completed in the CFG Hardening phase (M2)
before any per-language semantics work begins.

## 4. Shared Engineering Work

### 4.1 Capability Consistency Gate

Add a test that compares:

- `LanguageCapabilityProfile::all()`
- README supported-language table
- `docs/architecture.md` capability table
- `atlas doctor` language output

The test should fail when a language advertises a feature that the feature
matrix marks unsupported, or when docs omit a language.

### 4.2 Golden Fixture Ladder

Each language needs the same minimum fixture suite:

| Fixture | Purpose |
|---------|---------|
| `simple` | parser, symbols, references |
| `imports` | import/include extraction |
| `calls` | callsites and internal references |
| `class` / `struct` | type/member/container extraction |
| `dataflow_assign` | assignment RHS -> LHS |
| `dataflow_field` | field/property load and store |
| `summary_arg_to_param` | caller argument -> callee parameter |
| `summary_return_to_call` | callee return -> caller value |
| `cfg_if_else` | branch, true/false edges, join |
| `cfg_loop` | loop node, loop-back, exit edge |
| `semantic_resource` | producer -> local/field -> consumer |

If a language cannot support a fixture yet, keep a targeted failing/ignored test
with an explicit reason in the capability limitation.

### 4.3 CFG Hardening ✅ (completed M2)

**Status**: CFG body traversal verified for all 9 languages with CFG support.

The roadmap's claim of a `walk_if`/`walk_loop` body traversal bug was partially
outdated: the code in `cfg_builder.rs` already correctly recursed into branch/loop
bodies for most languages (TypeScript, JavaScript, Python, Java, C, C++). The
actual bug was language-specific wrapper nodes that `walk_block` did not handle:

| Issue | Language | Fix |
|-------|----------|-----|
| `statement_list` wrapper hides control-flow nodes | Go | Added handler in `walk_block` to recurse through `statement_list` |
| `expression_statement` wraps if/loop as plain Statement | Rust | Added handler to dispatch to `walk_if_node`/`walk_loop_node` |

CFG hardening deliverables:

1. ✅ Fixed `walk_block` to handle Go `statement_list` and Rust `expression_statement` wrappers.
2. ✅ Added `walk_if_node` and `walk_loop_node` methods for wrapper-node dispatch.
3. ✅ Added `cfg_if_else` and `cfg_loop` golden fixtures for 7 languages: TypeScript, Python, Go, Rust, Java, C, C++.
4. ✅ All `FeatureMatrix.cfg` limitation annotations removed; capability declarations restored to `supported_with_confidence`.
5. ✅ Added `test_cfg_body_traversal_if_else` and `test_cfg_body_traversal_loop` unit tests in `cfg_builder.rs`.
6. ✅ Extended `EscapeTarget` with `AsyncContext` variant.

Remaining CFG gaps (not blocking L5 semantics):
- try/catch/finally, switch, async/await, labeled break/continue — remain unsupported.
- ArkTS, C#, PHP, Ruby, Kotlin — CFG still `feature_unsupported()`.

## 5. Language-by-Language Plan

### 5.1 TypeScript

Current role: default language, high user value, shared path for JavaScript and
ArkTS-compatible syntax.

Primary gaps:

- Nested destructuring and async patterns are only partially verified.
- Barrel/re-export chains are best-effort.
- Framework semantics are absent outside generic resource patterns.
- CFG lacks advanced constructs such as try/catch/finally, switch, async/await.

Evolution:

1. Add fixtures for nested object/array destructuring, optional chaining,
   nullish coalescing, async functions, Promise chains, and re-export barrels.
2. Improve dataflow for destructuring by emitting one DataNode per target and
   preserving access paths from source object to target variable.
3. Extend import resolution with explicit export graph facts:
   `export { x } from`, `export * from`, default re-export, barrel chains.
4. Add TypeScript domain rule registry:
   - `resource_factory`: `open`, `createConnection`, `connect`. (Exclude `new`
     — it is too broad for TypeScript; specific constructor patterns should be
     registered individually, e.g., `new ReadableStream`.)
   - `resource_cleanup`: `.close`, `.dispose`, `.destroy`, `.release`.
   - `react_hook`: `useEffect`, `useMemo`, `useCallback`. (Exclude `useState` —
     it is a state binding, not an effect/side-effect boundary. `useState` does
     not allocate or release resources.)
   - `effect_cleanup`: return function from `useEffect` (note: this cannot be
     matched at callee level; requires CFG+DataFlow analysis. Mark as aspirational
     until CFG hardening is complete.)
5. Add a TypeScript framework consumer for React:
   - `useEffect` body is an async/reactive boundary.
   - cleanup return is a deferred cleanup edge (aspirational, needs CFG).
   - state setter calls are side effects but not resource frees.

Acceptance:

- `trace variable` works through destructuring.
- `path` and `impact` follow internal re-export barrels when target files are
  indexed.
- `branch_diff` can compare `.close()` / `.dispose()` cleanup across branches.
- React effects are reported with lower confidence and explicit provenance.

### 5.2 JavaScript

Current role: shares TypeScript adapter but lacks static type cues.

Primary gaps:

- CommonJS and ESM interop need stronger coverage.
- Dynamic property access and prototype patterns are low confidence.
- Callback-heavy APIs need better callsite attribution.

Evolution:

1. Split JavaScript fixtures from TypeScript fixtures for CommonJS:
   `require`, `module.exports`, `exports.foo`, dynamic `import()`.
2. Add callsite extraction for callback registrations:
   `app.get(path, handler)`, `promise.then(handler)`, event emitters.
3. Add domain rules:
   - `callback_registration`: Express/EventEmitter patterns.
   - `timer_resource`: `setTimeout`, `setInterval`, `clearTimeout`,
     `clearInterval`.
   - `resource_cleanup`: `.close`, `.destroy`, `.removeListener`.
4. Treat dynamic property access as `Indeterminate` with confidence downgrade,
   not as an absent result.

Acceptance:

- CommonJS import/export fixtures produce imports and internal edges.
- Timer lifecycle can be represented as allocate/consume.
- Callback registration appears in context and trace diagnostics.

### 5.3 Python

Current role: default language with CFG and relatively strong binding support,
but high runtime dynamism.

Primary gaps:

- Dynamic import and attribute lookup cannot be fully static.
- Context manager semantics are not yet first-class effects.
- Decorators and descriptors need better structural attribution.

Evolution:

1. Add fixtures for `with open() as f`, nested context managers, decorators,
   class methods, static methods, async functions, and import aliases.
2. Add Python domain rule registry:
   - `resource_factory`: `open`, `socket.socket`, `requests.Session`,
     `sqlite3.connect`.
   - `close_method`: `.close`, `.release`, `.disconnect`.
   - `context_manager`: `__enter__`, `__exit__`, `contextlib.contextmanager`.
   - `decorator_boundary`: route decorators, pytest fixtures, click commands.
3. Extend EffectComposer or a Python consumer to emit:
   - `Alloc` for `open()` assignment.
   - `Free` with `ConsumptionStyle::ContextManaged` at `with` scope exit.
   - explicit `.close()` as method-call consumption.
4. Keep dynamic attribute access as low-confidence `PlaceRef::Indeterminate`.

Acceptance:

- `with open() as f` does not report missing close.
- `f = open(); f.close()` reports balanced lifecycle.
- `f = open(); return f` reports escape to return value.
- Decorated functions remain findable by normal symbol search and context.

### 5.4 Java

Current role: strong static syntax and interprocedural dataflow, but no full
classpath/type-system modeling.

Primary gaps:

- Maven/Gradle/classpath resolution is not modeled.
- Try-with-resources should become a semantic resource boundary.
- Overload resolution is name-based/best-effort.

Evolution:

1. Add fixtures for packages, imports, nested classes, method overloads,
   constructors, generics, try-with-resources, and lambdas.
2. Improve import/container resolution for package-qualified names inside the
   indexed project.
3. Add Java domain rule registry:
   - `resource_factory`: `openStream`, `openConnection`, constructors for
     known `AutoCloseable` types.
   - `close_method`: `.close`, `.disconnect`, `.dispose`.
   - `scope_cleanup`: try-with-resources.
   - `async_boundary`: `CompletableFuture`, executor submission.
4. Implement a Java semantic consumer:
   - try-with-resources emits context-managed/deferred cleanup.
   - `.close()` emits method-call consumption.
   - constructors can emit resource production when rule-backed.

Acceptance:

- Internal package imports resolve across files.
- try-with-resources produces balanced lifecycle.
- Overloaded calls include confidence/provenance instead of pretending exact
  compiler resolution.

### 5.5 C

Current role: strongest existing semantic target with function-pointer support
and ownership/lifecycle focus.

Primary gaps:

- No full preprocessing or macro expansion.
- Function pointer flow is local and depth-limited.
- Pointer aliasing and union layout remain best-effort.

Evolution:

1. Stabilize existing C registry and learning flow before adding complexity.
2. Add fixtures for:
   - `malloc -> local -> struct field`.
   - `field -> free -> nullify`.
   - missing cleanup across branches.
   - out-parameter allocation.
   - function pointer dispatch through local assignment.
3. Improve DataFlowBuilder for pointer field access and out-params:
   `foo(&ptr)` and `foo(&obj->field)`.
4. Extend domain rules:
   - `out_param_alloc_fn`.
   - `borrowed_return_fn`.
   - `transfer_ownership_fn`.
   - `cleanup_fn`.
5. Keep macro behavior explicit:
   - indexed macro identifiers can produce references.
   - expanded semantics are unsupported unless source has expanded code.

Acceptance:

- `branch_diff` detects alloc/free asymmetry through local-to-field transfer.
- lifecycle proof mode can cite rule-backed evidence.
- function-pointer diagnostics include depth and confidence.

### 5.6 C++

Current role: C-like support plus C++ syntax, but templates/overloads/RAII are
not compiler-grade.

Primary gaps:

- Template instantiation, overload resolution, and ADL are not modeled.
- RAII/destructor semantics are not first-class.
- `new/delete` and smart pointers need separate treatment.

Evolution:

1. Add fixtures for constructors/destructors, `new/delete`, `unique_ptr`,
   `shared_ptr`, move operations, references, and templates.
2. Add C++ domain rule registry separate from C with explicit `language='cpp'`
   registration. **Implementation note**: `CppOwnershipRules::load` in
   `analysis/src/ownership_rules.rs` currently hardcodes `load(store, "c")`;
   this must be refactored to load C and C++ rules independently.
   - `alloc_fn`: `operator new`, `make_unique`, `make_shared`.
   - `free_fn`: `operator delete`, `delete`.
   - `raii_type`: user or builtin RAII type patterns.
   - `move_transfer`: `std::move`.
4. Add C++ semantic consumer:
   - `unique_ptr` owns and frees at scope exit.
   - `shared_ptr` is shared ownership, lower confidence.
   - destructor cleanup is `ConsumptionStyle::Implicit`.
5. Keep overload/template results marked best-effort.

Acceptance:

- `new` followed by `delete` is balanced.
- `unique_ptr` scope exit is not flagged as leak.
- raw pointer ownership remains conservative and provenance-rich.

### 5.7 ArkTS

Current role: TypeScript grammar fallback for ArkTS-compatible syntax.

Primary gaps:

- ArkTS-specific syntax and decorators are not fully verified.
- Capability profile should not overstate native ArkTS support.
- UI/component lifecycle semantics are absent.

Evolution:

1. Keep ArkTS explicitly marked as "via TypeScript grammar" until native grammar
   coverage exists.
2. Add fixtures for `@Component`, `@Entry`, `@State`, `@Prop`, `@Link`,
   `@Builder`, lifecycle callbacks, and class-as-struct fallbacks.
3. Add ArkTS domain rules:
   - `component_lifecycle`: `aboutToAppear`, `aboutToDisappear`.
   - `state_binding`: `@State`, `@Prop`, `@Link`, `@Provide`, `@Consume`.
   - `builder_boundary`: `@Builder`.
4. Reuse TypeScript dataflow where syntax is compatible; emit unsupported
   diagnostics where ArkTS syntax is not parsed by the TS grammar.

Acceptance:

- Component symbols and lifecycle methods appear in search/context.
- State decorator references are surfaced as structural facts or diagnostics.
- ArkTS results always include grammar provenance.

### 5.8 Go

Current role: high-priority language with clear resource and concurrency
patterns.

Primary gaps:

- Generic type parameters are not deeply modeled.
- `defer` should become a semantic cleanup edge.
- Goroutine/channel boundaries need explicit effect modeling.

Evolution:

1. Add fixtures for `defer f.Close()`, `os.Open`, `sql.Open`, `context.Context`,
   goroutines, channels, methods with receivers, and generics.
2. Add Go domain rule registry:
   - `resource_factory`: `os.Open`, `sql.Open`, `net.Dial`, `http.Get`.
   - `close_method`: `.Close`.
   - `defer_cleanup`: `defer x.Close()`.
   - `ctx_pass`: `context.Context` parameter propagation.
   - `goroutine_boundary`: `go fn(...)`.
3. Add Go semantic consumer:
   - `defer x.Close()` emits `Free` with `ConsumptionStyle::Deferred`.
   - goroutine launch emits `Escape { to: Thread }` for captured values.
     (**Prerequisite**: `EscapeTarget::Thread` must be added to
     `types/src/effects.rs`. Until then, use `EscapeTarget::Unknown` with a
     diagnostic note.)
   - channel send may emit escape-to-argument/unknown depending on precision.

Acceptance:

- `f, err := os.Open(); defer f.Close()` is balanced.
- Missing defer/close is visible in lifecycle.
- Values captured by goroutines are marked as escaping.

### 5.9 C#

Current role: DataflowFull via summaries but no CFG support.

Primary gaps:

- CFG unsupported.
- Partial classes across files are not merged.
- `using` / `IDisposable` semantics are absent.

Evolution:

1. Add C# CFG config and fixtures for `if`, loops, `using`, try/finally,
   lambdas, async/await, and partial classes.
2. Improve container resolution for namespaces, classes, partial class fragments,
   methods, and properties.
3. Add C# domain rule registry:
   - `resource_factory`: `OpenConnection`, `OpenRead`, constructors for
     disposable resources.
   - `dispose_method`: `.Dispose`, `.Close`.
   - `using_scope`: `using var`, `using (...)`.
   - `async_boundary`: `Task`, `await`.
4. Add C# semantic consumer:
   - `using` emits context-managed cleanup.
   - `Dispose` emits method-call consumption.
   - async boundaries are diagnostics first, not full async dataflow.

Acceptance:

- C# moves from no-CFG to verified basic CFG.
- `using` prevents false missing-cleanup findings.
- partial-class facts are linked or explicitly diagnosed as partial.

### 5.10 Rust

Current role: high-priority language with CFG and strong ownership conventions,
but no borrow-checker modeling.

Primary gaps:

- Macro bodies are not analyzed.
- Borrow checker/lifetime semantics are not modeled.
- Drop/RAII and unsafe boundaries need language-specific interpretation.

Evolution:

1. Add fixtures for `Box::new`, `Arc::new`, `Rc::new`, `Drop`, `drop(x)`,
   `std::mem::forget`, `ManuallyDrop`, `unsafe`, moves, borrows, and macros.
2. Add Rust domain rule registry:
   - `owned_constructor`: `Box::new`, `Arc::new`, `Rc::new`, `Vec::new`.
   - `drop_impl`: `Drop::drop`, `drop`.
   - `forget_fn`: `std::mem::forget`.
   - `unsafe_boundary`: `unsafe`, `transmute`, raw pointer operations.
   - `shared_owner`: `Arc`, `Rc`.
3. Add Rust semantic consumer:
   - owned values are implicitly consumed at scope exit.
   - `drop(x)` is explicit consumption.
   - `mem::forget(x)` is an escape/leak-like semantic event.
   - unsafe blocks lower confidence and appear in diagnostics.
4. Do not attempt full borrow checker semantics. Model only coarse ownership
   events with clear provenance.
   **Limitation**: `shared_owner` rules (`Arc`, `Rc`) use a reduced-precision
   model. The current `FieldLifecycleEngine` state machine (Unknown/MaybeLive/
   Assigned/Freed/Nullified/Escaped/Returned/Invalidated) cannot express
   "last `Arc` reference dropped" semantics. Shared ownership results should
   carry explicit reduced-confidence annotations.

Acceptance:

- `Box::new` local ownership balances at scope exit.
- `drop(x)` is explicit cleanup.
- `mem::forget(x)` is reported as ownership escape.
- unsafe usage appears in semantic impact context.

### 5.11 PHP

Current role: DataflowFull with known parameter DataNode gap and dynamic method
call limitations.

Primary gaps:

- Parameter DataNode extraction must be corrected.
- Namespace aliases and dynamic calls need confidence-aware handling.
- Resource functions are common and should be semantic effects.

Evolution:

1. Fix parameter DataNode extraction first; do not deepen semantics until this
   is stable.
2. Add fixtures for namespaces, `use` aliases, methods, dynamic method calls,
   closures, `fopen/fclose`, database connections, and exceptions.
3. Add PHP domain rule registry:
   - `resource_factory`: `fopen`, `curl_init`, `mysqli_connect`, `PDO`.
   - `close_method`: `fclose`, `curl_close`, `.close`, `disconnect`.
   - `dynamic_call`: variable function/method call patterns.
4. Add PHP semantic consumer:
   - procedural resource functions are explicit calls.
   - object method close/disconnect is method-call consumption.
   - dynamic calls emit low-confidence diagnostics unless rule-backed.

Acceptance:

- ArgToParam fixtures pass without expected failure.
- `fopen -> fclose` is balanced.
- dynamic method results include confidence/provenance.

### 5.12 Ruby

Current role: DataflowFull with block/yield gap and no CFG.

Primary gaps:

- Blocks and `yield` are core Ruby semantics.
- Resource management often uses blocks: `File.open(...) { |f| ... }`.
- Dynamic dispatch should remain low confidence.

Evolution:

1. Add CFG support for basic methods, `if`, loops, blocks, and rescue/ensure
   as future work.
2. Add fixtures for blocks, `yield`, modules, mixins, `File.open` block form,
   `.close`, and metaprogramming fallbacks.
3. Add Ruby domain rule registry:
   - `resource_factory`: `File.open`, `.new`, connection factories.
   - `close_method`: `.close`, `.disconnect`, `.dispose`.
   - `block_managed_resource`: `File.open { |f| ... }`.
   - `dynamic_dispatch`: `send`, `method_missing`.
4. Add Ruby semantic consumer:
   - block form is context-managed cleanup.
   - explicit `.close` is method-call consumption.
   - `send` and `method_missing` are diagnostics, not precise edges.

Acceptance:

- `File.open {}` does not report missing close.
- block parameter receives the opened resource in local dataflow.
- `send` calls are visible as low-confidence dynamic boundaries.

### 5.13 Kotlin

Current role: DataflowFull with extension receiver binding gap and no CFG.

Primary gaps:

- Extension functions and implicit receiver `this` need better binding.
- `use {}` is Kotlin's resource-management idiom.
- Coroutines create async boundaries.

Evolution:

1. Add CFG support for functions, classes, `if`, loops, `try/finally`, lambdas.
2. Add fixtures for extension functions, receivers, data classes, nullable
   operators, `use {}`, coroutines, and Java interop calls.
3. Add Kotlin domain rule registry:
   - `resource_factory`: `openConnection`, Java interop open methods.
   - `close_method`: `.close`, `.dispose`.
   - `use_scope`: `.use {}`.
   - `coroutine_boundary`: `launch`, `async`, `withContext`.
4. Add Kotlin semantic consumer:
   - `.use {}` emits context-managed cleanup.
   - extension receiver maps into a receiver place.
   - coroutine boundaries mark captured values as escaping to async context.

Acceptance:

- Extension receiver `this` binding is traceable.
- `.use {}` balances resource lifecycle.
- coroutine captures are marked as escape/boundary diagnostics.

### 5.14 Cangjie

Current role: DataflowFull but still young; CFG/profile consistency needs
cleanup.

Primary gaps:

- CFG capability declaration and implementation need alignment.
- `postfixExpression` / `callSuffix` method call handling is a known edge.
- Language-specific semantic conventions are not yet mature.

Evolution:

1. First align capability profile, docs, and actual CFG behavior.
2. Add fixtures for simple functions, structs/classes, imports, method calls,
   postfix call suffixes, dataflow assignment, return flow, and basic if/loop
   if grammar support is stable.
3. Keep domain rules minimal:
   - no builtin ownership/resource claims until real conventions are collected.
   - allow user rules for `resource_factory` and `close_method` only if the
     registry validates them as candidate or user-provided.
4. Prefer diagnostics over speculative semantics.

Acceptance:

- method calls are consistently captured.
- capability profile no longer contradicts docs.
- unsupported semantic features return diagnostics, not silent empty results.

## 6. Milestones

**Dependency note**: The L0-L7 capability ladder establishes a strict ordering:
L6 (Semantic Effects) depends on L5 (CFG), and L7 (Language Semantics) depends
on L6. Therefore CFG hardening (M2) MUST complete and be verified for a language
before any per-language semantics work begins for that language (M4-M6).

For languages in M6 (Managed Runtime) that lack CFG support (Java, C#, Kotlin,
Ruby, PHP): semantic analysis is limited to CFG-independent diagnostics (e.g.,
DataFlow-based resource operation matching). Full branch_diff/lifecycle requires
CFG and should be deferred or delivered with explicit reduced-precision
annotations.

### M1: Capability Truthfulness

Scope:

- Fix capability/documentation drift.
- Add consistency tests.
- Make `DataflowFull` wording precise in README and architecture docs.
- **Immediately downgrade CFG declarations**: all 8 languages currently claiming
  CFG support (TypeScript, JavaScript, Python, Java, C, C++, Go, Rust) must have
  `FeatureMatrix.cfg` annotated with limitation: "branch/loop body traversal not
  yet implemented; only CFG node topology (Branch/Loop/Join) is emitted."
  This is a fact-correction, not a regression.

Exit criteria:

- `cargo test -p atlas-cli --features "all-languages,mcp"` passes.
- Docs and capability profiles agree for every language.
- CFG limitation annotations are present for all applicable languages.

### M2: CFG Hardening ✅ (completed)

Scope:

- ✅ **Fixed `walk_block` in `extraction/src/cfg_builder.rs`**: Added handlers for
  Go `statement_list` wrapper and Rust `expression_statement` wrapper.
- ✅ **Added `cfg_if_else` and `cfg_loop` golden fixtures** for TypeScript, Python,
  Go, Rust, Java, C, and C++ (28 fixture files total).
- ✅ **Extended `EscapeTarget`** in `types/src/effects.rs` with `AsyncContext`
  variant.
- ✅ **Lifted all `FeatureMatrix.cfg` limitation annotations** for languages
  with verified body traversal.
- ✅ ArkTS/Cangjie CFG: Cangjie CFG remains supported; ArkTS remains unsupported
  (no tree-sitter parser).

Exit criteria met:
- CFG fixtures verify entry/exit, true/false branch, join, loop-back, and loop
  exit edges with body traversal for all 7 fixture languages.
- `walk_if`/`walk_loop` body traversal verified by fixtures.
- All `FeatureMatrix.cfg` fields restored to `supported_with_confidence` values.

### M3: Dataflow Gap Closure ✅ (completed)

Scope:

- ✅ **Fixed 7 outdated CFG limitation texts** in TypeScript, JavaScript, Java, C, C++,
  Cangjie, and Rust profiles — changed "branch/loop body traversal not yet implemented"
  to "Control-flow graph with branch/loop body traversal implemented".
- ✅ **Fixed outdated PHP comment** in capability.rs (fx15 should_panic → both bridges verified).
- ✅ **Raised 12 interprocedural_summaries confidence floors** to match language
  confidence_floor values: TypeScript (0.55→0.72), JavaScript (0.55→0.60),
  Java (stayed 0.75), C (0.60→0.73), C++ (0.60→0.70), ArkTS (0.55→0.60),
  Cangjie (0.55→0.65), C# (0.55→0.72), Rust (0.60→0.70), PHP (0.55→0.62),
  Kotlin (0.55→0.67), Ruby (stayed 0.65).
- ✅ **Updated consistency test** (`test_cfg_known_limitation`) to assert "body traversal
  implemented" not "body traversal not yet implemented".
- ✅ Summary fixtures (ArgToParam/ReturnToCall trace_fixtures) already existed for all
  14 languages — no new fixtures needed.

Exit criteria met:
- Zero occurrences of "branch/loop body traversal not yet implemented" in capability.rs.
- All 14 languages have ArgToParam + ReturnToCall (or verified gap) bridges documented.
- 172 CLI tests, 71 trace_fixtures, 50 golden tests, 103 types tests — all pass.
- No language labeled DataflowFull has a core fixture expected to fail without
  a documented capability downgrade.

### M4a: Rust + Go Resource Tracking ✅ (completed)

**Prerequisite**: M2 CFG hardening must be complete and verified for Rust and Go.

Scope:

- ✅ **Fixed `ResourceOpConfig::default_rust()`**: Replaced `Contains("::new")` with
  Exact matchers for `Box::new`, `Vec::new`, `Arc::new`, `Rc::new`,
  `std::sync::Arc::new`, `std::rc::Rc::new`. Added Exact consumers for `drop`,
  `std::mem::drop`, `std::mem::forget` (escape).
- ✅ **Fixed `ResourceOpConfig::default_go()`**: Replaced `Contains("Open")` with
  Exact matchers for `os.Open`, `os.Create`, `sql.Open`, `net.Dial`, `os.OpenFile`.
  Consumer uses `Suffix(".Close()")`.
- ✅ **Created `domain_rules/src/kinds/rust.rs`**: RustRegistry with rule kinds
  `rust/alloc_fn`, `rust/free_fn`, `rust/owned_pattern`, `rust/cleanup_fn` + builtin
  rules + stub RustLearningStrategy.
- ✅ **Created `domain_rules/src/kinds/go.rs`**: GoRegistry with rule kinds
  `go/alloc_fn`, `go/free_fn`, `go/escape_fn`, `go/cleanup_fn` + builtin rules
  + stub GoLearningStrategy.
- ✅ **Wired registries** into `kinds/mod.rs`.
- ✅ **Fixed `CppOwnershipRules::load`**: Added `Language` parameter; no longer
  hardcoded to `"c"`.
- ✅ **8 integration tests** in resource_ops.rs: test_rust_config_produces/consumes/classify,
  test_go_config_produces/consumes/classify.

Exit criteria met:
- Rust `Box::new`/`drop`/`mem::forget`: Alloc/Free/Escape effects classified by ResourceOpConfig.
- Go `os.Open`/`Close()`: Alloc/Free effects classified.
- 12 resource_ops tests pass; full suite zero regressions.

### M4b: Escape + Defer + Scope Exit ✅ (completed)

Scope:

- ✅ **Added `CallContext` enum** to `types/src/enums.rs`: `None`, `GoGoroutine`, `GoDefer`
  — language-agnostic call-site annotation, general-purpose for future languages.
- ✅ **Added `call_context` field** to `CfgNode` with `#[serde(default)]` for backward
  compatibility.
- ✅ **Go CFG builder**: Sets `GoGoroutine`/`GoDefer` when processing `go_statement`/
  `defer_statement` tree-sitter nodes. Implemented via `pending_call_context` in
  `CfgContext` + `process_go_defer_inner` helper.
- ✅ **`classify_escape` on OwnershipContract**: New trait method with default no-op impl.
  `ResourceOpConfig` impl: `GoGoroutine` context → `EscapeTarget::Thread`;
  explicit escape patterns (e.g., `std::mem::forget`) also checked.
- ✅ **`ConsumptionStyle::Deferred` for Go**: When `call_context == GoDefer`,
  `classify_consumption` returns `Deferred` style for matching consumer patterns.
- ✅ **`ScopeExitAnalyzer`** (new `analysis/src/scope_exit.rs`): Post-pass that
  collects unfreed Alloc effects and emits Free at the Exit CFG node. Conservative:
  frees all tracked allocations at function exit. Correct for Rust Drop semantics.
  Called from `compose_effects` after the main loop.
- ✅ **17 resource_ops tests** (9 new: Go/Rust escape, defer, scope-exit)
- ✅ **Updated 17 CfgNode struct literals** across branch_diff, lifecycle, store_rows,
  branching tests.
- ✅ **`std::mem::forget`**: Both consumer (Free) AND escape (Escape) patterns.

Exit criteria met:
- Go `goroutine escape`: `classify_escape` returns `EscapeTarget::Thread` for `GoGoroutine` context.
- Go `defer Close`: `ConsumptionStyle::Deferred` when `call_context == GoDefer`.
- Rust `scope exit`: `ScopeExitAnalyzer` emits Free at Exit for all unfreed Allocs.
- 292 total tests passing, zero regressions.

### M5a: Python + TypeScript Patterns & Registries ✅ (completed)

**Prerequisite**: M2 CFG hardening must be complete and verified for Python and
TypeScript.

Scope:

- ✅ **Enhanced `default_python()`**: Added `sqlite3.connect`, `socket.socket`,
  `requests.Session` producers; kept `Suffix("connect")` catch-all. Consumers
  unchanged (`.close`, `.dispose`, `.release`, `os.close`).
- ✅ **Enhanced `default_ts_js()`**: Added producers `createReadStream`,
  `createWriteStream`, `createServer`, `createClient` (Node.js factories);
  `useEffect`, `useMemo`, `useCallback` (React hooks); `setTimeout`, `setInterval`
  (timer factories). Consumers unchanged.
- ✅ **React hooks in `classify_return`**: `useEffect` → `ReturnContract::MaybeOwned`
  (effect subscription as resource). `useMemo`/`useCallback` → `NewOwned`. Moved
  MaybeOwned check before generic producer match.
- ✅ **Created `PythonRegistry`** in `domain_rules/src/kinds/python.rs`: rule kinds
  `python/alloc_fn`, `python/free_fn`, `python/context_manager`, `python/decorator_boundary`
  with builtin rules + `PythonLearningStrategy` stub.
- ✅ **Created `TypeScriptRegistry`** in `domain_rules/src/kinds/typescript.rs`: rule kinds
  `ts/alloc_fn`, `ts/free_fn`, `ts/react_hook`, `ts/cleanup_return` with builtin rules
  + `TypeScriptLearningStrategy` stub.
- ✅ **Wired registries** into `kinds/mod.rs`.
- ✅ **6 integration tests** in resource_ops.rs: test_python_config_produces/consumes/classify,
  test_ts_config_produces/consumes/classify.

Exit criteria met:
- Python `open`/`.close` lifecycle verified.
- Python `with open` implicitly handled by `ScopeExitAnalyzer` (Free at function exit).
- TypeScript `.dispose`/`.close` lifecycle verified.
- React `useEffect` classified as MaybeOwned; cleanup return (Deferred Free) implemented in M5b.
- Full test suite: 54 engine + 71 trace_fixtures + 50 golden + 50 IR + 92 ID store — all pass. Combined M5a+M5b: zero regressions.

### M5b: Context Manager & React Cleanup ✅ (completed)

Scope:

- ✅ **Added `CallContext::PythonWith`**: CFG builder sets this context on allocation
  calls inside `with_statement`.
- ✅ **Added `CfgNodeKind::BlockExit`**: Emitted at the end of `with` block body.
  `ScopeExitAnalyzer` finds nearest BlockExit for PythonWith Allocs and emits Free
  with `ConsumptionStyle::ContextManaged`.
- ✅ **Added `DataNodeKind::CleanupReturn`**: Capture for `return <arrow_function>` inside
  React callback bodies. EffectComposer marks Free effects as `Deferred` when
  CleanupReturn DataNode exists in the effect scope.
- ✅ **React cleanup detection**: Tree-sitter query capture in
  `queries/typescript/dataflow_builder.scm` + mapping in `typescript.rs`.
- ✅ **`SemanticEffect` enhanced**: Added optional `consumption_style` and `description`
  fields with serde defaults for backward compatibility.
- ✅ **DB schema**: `call_context` column added to `cfg_nodes` table + migration.
  Also fixed pre-existing bug where `semantic_effects_json` was missing from SELECT.
- ✅ **2 integration tests**: `test_python_with_lifecycle`, `test_ts_react_cleanup`.
- ✅ **54 engine + 71 trace_fixtures + 50 golden + 50 IR + 92 ID store** — all pass.

### M6b/c/d/e: C# + Kotlin + Ruby + PHP ✅ (completed)

Scope:

- ✅ **C# CFG**: From `unsupported` → `supported_with_limitations(0.72)`. Added
  `CfgLanguageConfig` with `using_statement` handler (identical pattern to
  Python/Java context manager). `CallContext::CSharpUsing` added.
- ✅ **Kotlin CFG**: From `unsupported` → `supported_with_limitations(0.67)`. Added
  `CfgLanguageConfig` with verified tree-sitter-kotlin v0.4.0 node kinds
  (`function_body`, `if_expression`, `for_statement`, `while_statement`).
- ✅ **`ScopeExitAnalyzer`**: `is_context_managed` handles `CSharpUsing` alongside
  `PythonWith`/`JavaTryWith`.
- ✅ **4 new domain registries**: `CSharpRegistry` (idisposable), `KotlinRegistry`
  (coroutine), `RubyRegistry` (block_resource), `PhpRegistry` (procedural_resource).
- ✅ **Enhanced ResourceOpConfig**: C# (File.Open, FileStream, SqlConnection),
  Kotlin (.use extension, bufferedReader), Ruby (File.open, TCPSocket, Net::HTTP),
  PHP (fopen, mysqli_connect, curl_init — exact consumers).
- ✅ **C#/Kotlin capability profiles**: `cfg` upgraded from unsupported, `FeatureMatrix`
  updated to include `cfg` in supported features.
- ✅ **8 resource_ops tests** + **8 integration tests** + **4 fixture files**.
- ✅ **Test results**: 48 domain-rules + 104 analysis + 29 integration + 25 golden
  + 33 trace_fixtures — 239 total, zero regressions.

Exit criteria met:
- C# `using`/IDisposable: Alloc → Free at BlockExit (ContextManaged).
- Kotlin `.use {}` lambda scope exit handled by ScopeExitAnalyzer.
- Ruby `File.open` with block handled by ScopeExitAnalyzer.
- PHP procedural resources: patterns + ScopeExitAnalyzer.
- All M6 languages have domain registries and ResourceOpConfig.

**Prerequisite**: Java CFG verified (0.75 confidence).

Scope:

- ✅ **Added `CallContext::JavaTryWith`** to types/src/enums.rs.
- ✅ **Added `try_with_resources_statement` handler** in cfg_builder.rs — walks resource
  specifications, walks body block, emits `CfgNodeKind::BlockExit` at try block end.
  Placed before the generic `try_statement` handler (plain try-catch-finally unchanged).
- ✅ **`ScopeExitAnalyzer`**: Renamed `is_python_with` → `is_context_managed`; handles
  both `PythonWith` and `JavaTryWith` → Free at BlockExit with `ConsumptionStyle::ContextManaged`.
- ✅ **Enhanced `default_java()`**: Added `newInputStream`, `newOutputStream`, `getConnection`
  producers; consumers `.close()`, `.dispose()`, `.destroy()`.
- ✅ **Created `JavaRegistry`** in domain_rules/src/kinds/java.rs: rule kinds
  `java/alloc_fn`, `java/free_fn`, `java/try_resource`, `java/cleanup_fn` with builtin rules.
- ✅ **Wired registries**: `kinds/mod.rs` + analysis/java feature flag.
- ✅ **3 resource_ops tests** + **1 golden fixture** (try_resource.java) + **1 integration test**.
- ✅ **Test results**: 36 domain-rules + 99 analysis + 26 golden + 23 integration — zero regressions.

Exit criteria met:
- Java try-with-resources lifecycle: Alloc at constructor → Free at BlockExit (ContextManaged).
- Java `.close()` explicit consumption verified.

**Prerequisite**: For full semantics (branch_diff/lifecycle), each language must
have verified CFG support from M2. Java and C# require CFG hardening; Kotlin,
Ruby, and PHP may deliver CFG-independent diagnostics first.

Scope:

- Add Java and C# CFG support if not already completed in M2.
- Add Java and C# resource semantics after CFG support is verified.
- Add Kotlin `.use`, Ruby block-managed resources, PHP procedural resources.
- For languages without CFG: deliver DataFlow-based resource matching only, with
  explicit reduced-precision annotations.

Exit criteria:

- Managed-language resource idioms produce balanced lifecycle when used
  correctly and diagnostics when omitted.
- Results for CFG-unsupported languages include precision/provenance metadata.

## 7. Implementation Checklist Per Language

For every language capability change:

1. Update tree-sitter query or language adapter.
2. Update `DataFlowBuilder` or `CfgBuilder` if needed.
3. Update `LanguageCapabilityProfile`.
4. Add or update golden fixture.
5. Add or update trace/e2e fixture.
6. Add or update domain rule registry if semantic rules are involved.
7. Add analysis consumer tests if semantics are involved.
8. Update README and architecture capability table.
9. Ensure MCP responses include capability/provenance/diagnostics.

## 8. Architecture Decision Record

This section records irreversible or high-cost architectural decisions made during
the review of this plan, with rationale and alternatives considered. These
decisions are binding on all implementation work.

### ADR-1: No parallel L0-L7 enum

**Decision**: Reject introducing a new L0-L7 capability enum into the type system.
**Rationale**: `FeatureMatrix` (13 typed fields) already provides finer granularity
than an 8-level enum. A parallel model creates dual-track maintenance. L6/L7 are
analysis-layer capabilities, not extraction-layer, and should use a separate
`SemanticCapability` model.
**Alternatives considered**: Add `L0-L7` as an enum alongside `CapabilityLevel`.
Rejected for maintenance burden and category confusion.

### ADR-2: Extend OwnershipContract, not create LanguageSemanticConsumer

**Decision**: Extend the existing `OwnershipContract` trait with `classify_boundary`
rather than introducing a parallel `LanguageSemanticConsumer` trait.
**Rationale**: `EffectComposer` already consumes `&dyn OwnershipContract`. Adding a
parallel trait creates dual integration paths and risks inconsistent effect
composition. Each language's consumer is an `impl OwnershipContract`.
**Alternatives considered**: New `LanguageSemanticConsumer` trait. Rejected for
fragmenting the analysis pipeline.

### ADR-3: CFG hardening precedes all semantics

**Decision**: No per-language semantic analysis (M4-M6) may proceed for a language
until its `walk_if`/`walk_loop` body traversal bug is fixed AND verified by
golden fixtures for that language.
**Rationale**: L6 depends on L5. Delivering semantic effects without verified CFG
produces silently incorrect results. The current `walk_if`/`walk_loop` placeholder
is a structural bug, not a missing feature.
**Alternatives considered**: Deliver semantics in parallel with CFG fixes.
Rejected for correctness risk.

### ADR-4: CFG capability declarations downgraded, then restored ✅

**Decision**: All 8 languages were downgraded to `supported_with_limitations` pending
CFG body traversal verification. After fixing Go/Rust wrapper-node bugs and adding
golden fixtures for all 7 core languages, all limitations were lifted and capability
declarations restored to `supported_with_confidence`.
**Finding**: The roadmap's claim of a universal `walk_if`/`walk_loop` body traversal
bug was outdated. Most languages already had correct traversal; only Go (`statement_list`)
and Rust (`expression_statement`) wrapper nodes broke the `walk_block` dispatch.
**Alternatives considered**: Wait for fix, then update. Rejected; downgrade-first
was the honest path.

### ADR-5: EscapeTarget extended in M2 ✅

**Decision**: `EscapeTarget` enum in `types/src/effects.rs` gained `AsyncContext`
variant during CFG Hardening. `Thread` already existed in the enum.
**Rationale**: Go goroutines and Kotlin coroutines need `Thread` for goroutine
escape; async/await needs `AsyncContext` for coroutine-local escape analysis.
**Alternatives considered**: Use `EscapeTarget::Unknown` indefinitely. Rejected
for loss of precision.

### ADR-6: FeatureMatrix is the single authoritative source

**Decision**: `FeatureMatrix` in `types/src/capability.rs` is the canonical
source of per-language capability truth. All docs, MCP responses, and tests
must derive from or be checked against it.
**Rationale**: Avoids documentation drift. The consistency gate (M1) enforces
this.
**Alternatives considered**: Manual capability tables in docs. Rejected for
inevitable drift.

## 9. Non-Goals

- No compiler-grade type checking.
- No full C/C++ preprocessing or template instantiation.
- No full Python/Ruby/PHP runtime dispatch.
- No full Java/C# classpath resolution.
- No automatic SAST finding generation.
- No cross-language universal ownership semantics.

Atlas should remain an explainable, bounded, local-first semantic graph. When a
language feature is best-effort, the result must say so with confidence,
strategy, and provenance.
