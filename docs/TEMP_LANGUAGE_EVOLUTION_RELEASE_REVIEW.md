# Language Evolution Release Review

Date: 2026-06-04

Scope:
- Release-prep review after the latest fixes.
- Covers non-lazy and lazy paths, CLI, MCP, TUI, and all project languages.
- Excludes CodeGraph-related content because it is outside this project scope.

Method:
- Review from user-facing entry points down to extraction/storage/analysis contracts.
- Check all analysis levels: Manifest, ResolutionSymbols, Structural, Full.
- Check both eager and lazy analysis paths.
- Check every compiled language entry for extraction, lazy loading, capability profile, and test fixture visibility.

## Pass 1 - Entry Points And Shared Contracts

Status: completed.

### Findings

1. **[P1] Shared `run_index_pipeline` does not remove deleted files, so MCP `index` can leave stale symbols for files that no longer exist.**

   Evidence:
   - `crates/atlas-mcp/src/tools/index.rs` routes both foreground/background MCP indexing through `run_index_pipeline(...)`.
   - `crates/atlas-engine/crates/filesync/src/index_pipeline.rs:112-114` initializes frontends and then calls `phase_cleanup_stale(store, &discovered)`.
   - `phase_cleanup_stale` deletes exactly the paths passed to it. Passing `discovered` deletes/replaces currently existing files, but it does not discover DB rows for files removed from disk.
   - The CLI path does the missing step: `crates/atlas-cli/src/commands/index.rs:146-159` calls `phase_dirty_check(...)` and then cleans `hash_result.deleted`.

   Impact:
   - MCP manifest index can report deleted files/symbols as still present.
   - Downstream MCP search/context/trace structural lazy paths can start from stale manifest candidates.
   - This violates the release testing principle that shared pipeline and user entry points must be validated separately.

   Required fix before release:
   - Add dirty/deleted-file cleanup to `run_index_pipeline`, or document and rename the function so MCP does not use it for authoritative indexing.
   - Add an MCP/shared-pipeline regression test: index a file, delete it, run MCP/shared manifest index again, assert the file and symbols are gone.

2. **[P1] Shared `run_index_pipeline(Full)` still does not build persistent function summaries.**

   Evidence:
   - `crates/atlas-engine/crates/filesync/src/index_pipeline.rs:193-218` resolves/builds graph, materializes annotations, emits completion, and returns.
   - There is no call to `phase_build_summaries(...)` in this Full path.
   - The CLI `atlas index --analysis full` path does call summaries only when `mode.produces_dataflow()` is true at `crates/atlas-cli/src/commands/index.rs:366-375`.

   Impact:
   - The same Full analysis level produces different persisted state depending on entry point.
   - `docs/testing.md` requires `shared pipeline Full` to persist structural + dataflow + CFG + summaries.
   - Any future caller using the shared pipeline for Full will silently miss inter-procedural summary facts.

   Required fix before release:
   - In `run_index_pipeline`, after annotation materialization, call `phase_build_summaries(store)` when `options.mode.produces_dataflow()` is true.
   - Add a shared-pipeline Full test that asserts summary rows exist after Full indexing.

3. **[P1] Several languages still use full definition queries for Manifest mode, so Manifest can over-index non-top-level symbols.**

   Evidence:
   - `SymbolExtractorSpec::manifest_query()` defaults to `definition_query()` at `crates/atlas-engine/crates/extraction/src/frontend.rs:84-90`.
   - The following language adapters do not override `manifest_query()`:
     - JavaScript
     - Cangjie, even though `queries/cangjie/manifest.scm` exists
     - CSharp
     - Kotlin
     - PHP
     - Ruby
   - The languages that do override it are TypeScript, Python, Java, C, C++, Go, Rust, and ArkTS.

   Impact:
   - Manifest is the MCP index path and TUI initial-index concept.
   - For affected languages, Manifest mode may write local variables/method-internal symbols rather than top-level declarations only.
   - This breaks the expected upgrade ladder: Manifest -> Structural -> LazyDataflow/Full.

   Required fix before release:
   - Either add dedicated manifest queries for each affected language, or make the default manifest query empty/unsupported and explicitly opt languages in.
   - Add per-language Manifest tests that assert no local/function-body symbols are emitted.

4. **[P2] CLI `--analysis` accepts arbitrary strings and silently falls back to Structural.**

   Evidence:
   - CLI args define `analysis: String` without value validation at `crates/atlas-cli/src/lib.rs:96-98` and `crates/atlas-cli/src/lib.rs:104-106`.
   - `index` maps unknown values to Structural at `crates/atlas-cli/src/commands/index.rs:39-43`.
   - `sync` maps unknown values to Structural at `crates/atlas-cli/src/commands/sync.rs:13-17`.

   Impact:
   - A typo such as `--analysis ful` runs Structural while the user believes Full was requested.
   - Release validation of Manifest/Full can be invalidated by a typo without an error.

   Required fix before release:
   - Use a clap `ValueEnum` or explicit validation for `manifest | structural | full`.
   - Return an error for unknown values.

5. **[P2] CFG-consuming MCP lazy tools can return a successful CFG analysis while nested `analysis_contract` says CFG is not available.**

   Evidence:
   - `branch_diff` triggers `ensure_for_function`, re-queries CFG, and runs analysis when CFG nodes are present at `crates/atlas-mcp/src/tools/branch_diff.rs:59-65` and `crates/atlas-mcp/src/tools/branch_diff.rs:201-205`.
   - `lifecycle` follows the same pattern at `crates/atlas-mcp/src/tools/lifecycle.rs:68-86` and `crates/atlas-mcp/src/tools/lifecycle.rs:224-228`.
   - `LazyDataflowService` intentionally omits CFG from `LazyWindow.capability_mask` because CFG is language-specific.
   - `LazyDiagnostics::from_layers(None, Some(window), None)` builds the contract only from that window-level mask.

   Impact:
   - For languages where CFG was actually built and consumed, the response can include branch/lifecycle results while the diagnostics warn that branch-level control flow cannot be analyzed.
   - This is a user-facing contract contradiction, not an extraction failure.

   Required fix before release:
   - For CFG-specific tools, either enrich the diagnostics with a tool-local proven CFG bit after `cfg_nodes` are found, or include unit/file capability state in `LazyWindow` so `from_layers` can aggregate it accurately.

6. **[P2] MCP `trace_variable` still hides dataflow lazy diagnostics when no trace path is returned.**

   Evidence:
   - `Engine::trace_variable` always attaches `resp.lazy_summary` after lazy dataflow is attempted at `crates/atlas-engine/src/lib.rs:384-442`.
   - MCP only converts `resp.lazy_summary` into `lazy_diagnostics` inside `if let Some(ref _path) = resp.result` at `crates/atlas-mcp/src/tools/trace.rs:269-282`.

   Impact:
   - If lazy dataflow was triggered but raw trace returns no path, the response still has diagnostics/partial state in the engine envelope, but the MCP top-level `lazy_diagnostics` and `analysis_contract` are omitted.
   - This weakens the no-result path exactly where users need diagnostics most.

   Required fix before release:
   - Remove the `resp.result` gate and convert `resp.lazy_summary` whenever it is present.
   - Add a regression test for no-path trace_variable that still asserts `lazy_diagnostics.dataflow`.

### Confirmed Good

- CLI `atlas index --analysis full` gates summary building on `mode.produces_dataflow()`, so Structural does not attempt dataflow summaries.
- CLI `atlas sync --analysis full` performs an incremental summary rebuild for changed files after sync.
- MCP `index` intentionally rejects the `analysis` parameter and stays Manifest-only.
- TUI auto-index is Manifest-only and has its own dirty/deleted cleanup path.
- Unit-level lazy dataflow state now gates CFG by language profile and actual CFG nodes, including prebuilt full-index cache hits.
- `from_dataflow_summary(...)` no longer uses the default zero capability mask; it reports manifest + structural + call_edges + dataflow.

## Pass 2 - Release Verification Commands

Status: completed.

### Commands Run

1. `cargo check -p atlas-cli --features all-languages,mcp`
   - Result: **pass**.
   - Meaning: the main CLI + MCP + all-language release build surface type-checks.

2. `cargo test -p atlas-mcp lazy_response`
   - Result: **pass**.
   - Covered: MCP lazy response contract unit tests.

3. `cargo test -p types capability --features typescript`
   - Result: **pass**.
   - Covered: capability profile tests when at least one language feature is enabled.

4. `cargo test -p types capability`
   - Result: **fail**.
   - Failure:
     - `capability::tests::test_all_profiles_are_valid`
     - `capability::tests::test_cfg_feature_matrix_consistent_with_supported_features`
   - Cause: with no language feature enabled, `LanguageCapabilityProfile::all_compiled()` returns an empty vector, but these tests assert at least one compiled profile.
   - Release impact: `docs/testing.md` says default feature combinations must compile/test. Either default features must include a language, or these tests must be feature-gated/rewritten for the no-language package mode.

5. `cargo test -p atlas-cli p3_capability_mask_cfg_gated_by_language --features all-languages`
   - Result: **fail before target test runs**.
   - Failure: `crates/atlas-cli/tests/integration.rs` does not compile because several hand-built `SemanticEffect` initializers are missing the new `eligible_for_implicit_cleanup` field:
     - `crates/atlas-cli/tests/integration.rs:1860`
     - `crates/atlas-cli/tests/integration.rs:1982`
     - `crates/atlas-cli/tests/integration.rs:2134`
     - `crates/atlas-cli/tests/integration.rs:2290`
     - `crates/atlas-cli/tests/integration.rs:2306`
   - Release impact: all `atlas-cli` integration-test based release validation is currently blocked.

### Additional Findings From Verification

7. **[P0] `atlas-cli` integration tests do not compile after `SemanticEffect` gained `eligible_for_implicit_cleanup`.**

   Evidence:
   - `SemanticEffect` defines `eligible_for_implicit_cleanup: Option<bool>` at `crates/atlas-engine/crates/types/src/effects.rs`.
   - The listed integration-test initializers still omit the field.

   Impact:
   - The targeted lazy CFG mask regression could not run.
   - Any release gate that runs `cargo test -p atlas-cli` fails at compile time.

   Required fix before release:
   - Add `eligible_for_implicit_cleanup: None` or the semantically correct value to the affected manual fixtures.
   - Re-run `cargo test -p atlas-cli p3_capability_mask_cfg_gated_by_language --features all-languages`.

8. **[P2] `types` capability tests fail in the default no-language feature configuration.**

   Evidence:
   - `cargo test -p types capability` fails because `LanguageCapabilityProfile::all_compiled()` is empty when no language feature is enabled.
   - `cargo test -p types capability --features typescript` passes.

   Impact:
   - The default package test configuration does not satisfy the documented release matrix.

   Required fix before release:
   - Either add a default language feature for `types`, or gate the “at least one compiled language” assertions behind a feature condition.

9. **[P2] Summary capability is built as data, but not reflected in persisted capability masks.**

   Evidence:
   - `CapabilityMask::SUMMARIES` exists and `AnalysisContract` checks it.
   - `phase_build_summaries(...)` writes summary tables through `SummaryStore::build_all(...)`, but does not update `extraction_state`.
   - `insert_file_facts(...)` derives file-level masks from `facts.layer` and optionally CFG nodes at `crates/atlas-engine/crates/db/src/store_writers.rs:728-731`; it never sets `SUMMARIES`.

   Impact:
   - Even after CLI Full builds summaries, DB-derived capability/status cannot prove `SUMMARIES`.
   - `AnalysisContract` will keep warning that inter-procedural summary tracing is unavailable unless some other caller manually sets the bit.

   Required fix before release:
   - After successful summary build, persist a summary capability signal, either as a dedicated extraction_state layer (`summaries`) or as an explicit bit update on affected file/function states.
   - Add a Full-mode test that asserts user-visible analysis_contract includes summary capability only after summaries are actually built.

## Pass 3 - All-Language Path Matrix

Status: completed.

### Language Entry Coverage

All 14 project languages are present in the major language entry lists:

| Language | extraction `create_frontend` | lazy frontend cache | capability profile | fixture dir | Manifest override |
|----------|------------------------------|---------------------|--------------------|-------------|-------------------|
| TypeScript | yes | yes | yes | yes | yes |
| JavaScript | yes | yes | yes | yes | no |
| Python | yes | yes | yes | yes | yes |
| ArkTS | yes | yes | yes | yes | yes |
| Java | yes | yes | yes | yes | yes |
| C | yes | yes | yes | yes | yes |
| C++ | yes | yes | yes | yes | yes |
| Cangjie | yes | yes | yes | yes | no, although `queries/cangjie/manifest.scm` exists |
| Go | yes | yes | yes | yes | yes |
| CSharp | yes | yes | yes | yes | no |
| Rust | yes | yes | yes | yes | yes |
| PHP | yes | yes | yes | yes | no |
| Ruby | yes | yes | yes | yes | no |
| Kotlin | yes | yes | yes | yes | no |

### Analysis-Level Path Review

| Level | CLI | MCP | TUI | Lazy | Release status |
|-------|-----|-----|-----|------|----------------|
| Manifest | `atlas index --analysis manifest` and `atlas sync --analysis manifest` exist | MCP `index` is Manifest-only | auto-index is Manifest-only | lazy structural starts from manifest candidates | blocked by missing per-language Manifest overrides and shared-pipeline deleted-file cleanup |
| ResolutionSymbols | internal dependency/lazy-resolution mode exists | no direct user tool | no direct TUI path | dependency/lazy resolution only | needs focused regression if touched; no new direct blocker found |
| Structural | default CLI index/sync | scoped MCP search/context/trace ensure structural | TUI no-command startup runs CLI Structural when DB missing | lazy structural service | generally wired; shared pipeline deletion gap can poison MCP structural candidates |
| LazyDataflow | no direct CLI user mode | `trace_variable`, `branch_diff`, `lifecycle`, `resume` dataflow paths | no direct TUI path | primary lazy path | unit-level state improved; MCP diagnostics still have no-path and CFG-contract gaps |
| Full | `atlas index --analysis full`; `atlas sync --analysis full` | MCP `index` intentionally rejects Full | no direct TUI path | full-index cache can satisfy lazy units | shared pipeline Full summary gap; summary bit not represented in file capability mask |

### Additional Observation

- `CapabilityMask::SUMMARIES` exists and `AnalysisContract` checks it, but normal file-level writes derive masks from `facts.layer` and CFG nodes only. Summary building writes summary tables but does not set a `summaries` extraction-state layer or bit. Even where CLI Full builds summaries, user-visible `analysis_contract` has no DB-derived way to prove `SUMMARIES`.
- This is lower priority than the missing summary build in shared Full, but it should be addressed before claiming Full exposes complete capability status.
