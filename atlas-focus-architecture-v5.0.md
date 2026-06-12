# Atlas Focus-driven Incremental Analysis — Implementation Plan v5.0

> **Scope**: Atlas only. Corpus reuse path confirmed via `SourceUniverse` trait (see v4.0).
> **Strategy**: Focus is the next control plane for the existing lazy execution layer.
> **Constraint**: Focus is internal infrastructure. Zero user-facing surface.
> No CLI commands, no manual pre-warm, no coverage dashboard. Activation is
> silent and automatic when the project has no full index.

---

## 0. Architecture Relationship: Focus vs Lazy

Focus does **not** replace lazy extraction with a second implementation.
Focus replaces the scattered query-time lazy **control plane**.

```text
Lazy = demand-built fact execution layer
Focus = query-intent scheduling, closure planning, visibility, precision, and background expansion
```

The existing lazy layer already owns the mechanics that must remain stable:

- `ExtractionMode::{Manifest, ResolutionSymbols, Structural, LazyDataflow, Full}`
- `extraction_state` freshness and capability masks
- `extraction_jobs` in-flight deduplication and task observability
- cancellable structural extraction
- unit/function-scoped dataflow extraction
- high-level `Engine` vs raw analysis separation

Focus must not fork those mechanics. It should drive them through one runtime:

```text
MCP Tool
  → QueryIntent
  → FocusRuntime::prepare(intent)
      → BootstrapManager
      → SeedLocator
      → ClosureEngine
          → LazyStructuralService
          → LazyDataflowService
      → ScopedResolver
      → FocusGraphBuilder
      → FocusResponseBuilder
```

The old lazy services become execution engines under `FocusRuntime`. The old
lazy orchestration entry points are removed after their responsibilities are
migrated.

**Preserve as long-term boundaries**

| Boundary | Why it stays |
|----------|--------------|
| `LazyStructuralService` | Builds structural facts for selected files; reuses cache, stale checks, cancellation, and extraction modes. |
| `LazyDataflowService` | Builds unit/function dataflow facts; preserves unit-scoped `extraction_state` and dataflow cache semantics. |
| `ExtractionMode` | Defines persistent fact layers. Focus chooses modes; it does not redefine them. |
| `extraction_state` | Source of truth for freshness and capability masks. |
| `extraction_jobs` | Source of truth for in-flight dedup, pending state, and task observability. |
| Raw analysis engines | Consume existing facts only. High-level runtime prepares facts first. |

**Remove or internalize as old control-plane boundaries**

| Boundary | Target |
|----------|--------|
| `LazyOrchestrator` | Delete after `FocusRuntime` owns policy, budget, diagnostics, and prewarm scheduling. |
| `LazyCoordinator` | Move useful responsibilities into `FocusRuntime` / `ClosureEngine` / `BootstrapManager`, then delete or make private implementation detail. |
| MCP `ensure_structural_for_files/name` helpers | Replace with `FocusRuntime::prepare(QueryIntent)`. |
| `PrecisionTier` as primary response semantic | Delete from public responses; `Precision { coverage, confidence }` is authoritative. |

The core invariant:

> Lazy builds facts. Focus decides which facts are needed, in what order, in
> which closure scope, under which visibility gate, and with what precision
> contract.

---

## 0.1 What Changes vs What Stays

| Existing | Action |
|----------|--------|
| `InvestigationFocus` (Symbol/Field/Position) | **Upgrade** → `FocusSeed` (add `File` variant + `language` field) |
| `Investigation` | **Upgrade** → `FocusWindow` (add budget + strategies + max_iterations) |
| `ClosurePlanner` | **Extend** → `ImportNeighborhood` strategy in the closure strategy catalog |
| `LazyBudget` | **Extend** → `WindowBudget` (add symbol/edge/fanout/bytes limits) |
| `LazyCoordinator` | **Move responsibilities** → `FocusRuntime` + `ClosureEngine` + `BootstrapManager`; delete/internalize afterward |
| `LazyOrchestrator` | **Replace** → `FocusRuntime` as the single query-time control plane |
| `PrecisionTier` (6-tier enum) | **Replace** → `Precision { coverage, confidence }`; no public adapter |
| `LazyStructuralService` | **Keep as execution engine**, called only under FocusRuntime/ClosureEngine |
| `LazyDataflowService` | **Keep as execution engine**, called only under FocusRuntime/ClosureEngine |
| `CandidateProvider` trait | **Keep**, becomes Tier 1 `SymbolHints` implementation |
| `extraction_jobs` table | **Extend** → add `closure_id`, `generation` columns |
| `symbols`, `references`, `symbol_edges` tables | **Keep global semantics**; focus does not write `references.resolved_*` or unscoped global graph state |
| `extraction_state` table | **Untouched** |
| `files` table | **Untouched** |

---

## 1. Phase 0: Precision Replacement (1 week)

Replace public response precision semantics directly. MCP responses must not
return both `precision_tier` and `precision`.

### New types in `types/src/structs.rs`

```rust
pub enum CoverageTier {
    RepoComplete,
    ClosureComplete { closure_id: String },
    Boundary { target_tier: SymbolTier },
    Partial { gaps: Vec<KnownGap> },
    Manifest,
}

pub enum SemanticConfidence {
    Certain, High, Medium, Low,
}

pub struct Precision {
    pub coverage: CoverageTier,
    pub confidence: SemanticConfidence,
}
```

### Adapter in `atlas-engine/src/compat.rs`

```rust
impl From<Precision> for PrecisionTier {
    fn from(p: Precision) -> PrecisionTier {
        match (p.coverage, p.confidence) {
            (CoverageTier::RepoComplete | CoverageTier::ClosureComplete { .. }, 
             SemanticConfidence::Certain) => PrecisionTier::Exact,
            (CoverageTier::ClosureComplete { .. }, _) => PrecisionTier::PartialExact,
            (CoverageTier::Boundary { .. }, SemanticConfidence::High) => PrecisionTier::PartialExact,
            (CoverageTier::Boundary { .. }, _) => PrecisionTier::DegradedStructural,
            (CoverageTier::Partial { .. }, _) => PrecisionTier::LocalDataflowOnly,
            (CoverageTier::Manifest, _) => PrecisionTier::ManifestOnly,
            _ => PrecisionTier::Unavailable,
        }
    }
}
```

### MCP tools: return only the public analysis envelope

```rust
struct McpResponse {
    analysis: AnalysisView,
    precision: Precision,
    coverage_counts: CoverageCounts,
    gaps: Vec<KnownGap>,
    work: WorkView,
    // ...
}
```

`precision_tier` is not part of the public MCP contract and must be removed
from default tool responses when the analysis envelope is introduced. If a
local conversion helper is still needed internally, it must stay private and
must not appear in MCP JSON.

---

## 2. Phase 1: Bootstrap — Cold Start Tiers (2 weeks)

### 2.1 Tier 0: `file_inventory` table

Separate from `files` table (which requires `content_hash NOT NULL`). Cheap stat. No parsing.

```sql
CREATE TABLE file_inventory (
    file_id BLOB PRIMARY KEY,
    path TEXT NOT NULL,
    language TEXT NOT NULL,
    mtime INTEGER NOT NULL,
    size INTEGER NOT NULL,
    inode INTEGER NOT NULL,
    dev INTEGER NOT NULL,
    discovered_at TIMESTAMP DEFAULT (datetime('now')),
    content_hash TEXT,           -- NULL until Tier 0.5 fills it
    last_fingerprinted_at TIMESTAMP
);
```

Filled lazily by the MCP process on first query for a project. It may reuse
the same discovery primitives as `filesync::phase_discover`, but there is no
`atlas open` daemon and no user-visible bootstrap command. Cost: a single
`readdir` walk. For 60K files on modern SSD: ~0.3s.

### 2.2 Tier 0.5: Content Fingerprints (background)

Async blake3 hashing. Priority: files in active focus closures → hot directories → rest.

```rust
// in atlas-engine/src/focus/atlas/bootstrap.rs
struct FingerprintWorker {
    store: Arc<Store>,
    scheduler: Arc<FocusScheduler>,
}

impl FingerprintWorker {
    async fn run(&self) {
        loop {
            let batch = self.next_priority_batch(64);
            for (file_id, path) in batch {
                let hash = blake3::hash(&fs::read(&path)?).to_hex();
                self.store.set_file_fingerprint(file_id, &hash);
            }
        }
    }
}
```

### 2.3 Tier 1: SymbolHints (background)

Reuses existing `CandidateProvider` trait (FTS5 + ripgrep). Building the hint index:

```rust
struct SymbolHintsBuilder {
    store: Arc<Store>,
    candidate_provider: Box<dyn CandidateProvider>,
}

impl SymbolHintsBuilder {
    async fn build_hot_paths(&self, inventory: &[FileId]) {
        for file_id in inventory {
            // Extract manifest symbols using existing extraction pipeline
            let file_facts = extract_file_with_mode(
                file_id, ExtractionMode::Manifest, &self.store
            )?;
            for symbol in &file_facts.symbols {
                self.hints.insert(symbol.name.clone(), HintEntry {
                    file_id,
                    kind: symbol.kind,
                    line: symbol.range.start.line,
                    confidence: 0.9, // manifest parser = high confidence
                });
            }
        }
    }
}
```

Hints are stored in-memory (dashmap) + persisted as `symbol_hints` table for session restart.

**Semantics: hints are hints, not truth.**
- Missing from hints → NOT "not found."
- False positives possible (name collisions).
- Source recorded for downstream confidence.

---

## 3. Phase 2: FocusRuntime + Focus Primitives (3 weeks)

### 3.0 FocusRuntime: the only query-time control plane

MCP tools must not directly call `LazyOrchestrator`, `LazyCoordinator`,
`LazyStructuralService`, `LazyDataflowService`, `ReferenceResolver`, or
`GraphBuilder`. They produce `QueryIntent` and call `FocusRuntime::prepare`.

```rust
pub enum QueryIntent {
    Calls { symbol: SymbolSelector, direction: Direction, depth: u32 },
    Explore { symbol: SymbolSelector },
    Search { query: String, scope: Option<PathScope> },
    TracePoint { file: FileRef, line: u32, column: u32 },
    TraceVariable { file: FileRef, line: u32, column: u32 },
    Context { symbol: SymbolSelector },
}

pub struct FocusRuntime {
    store: Arc<Store>,
    bootstrap: BootstrapManager,
    seed_locator: SeedLocator,
    closure_engine: ClosureEngine,
    scoped_resolver: ScopedResolver,
    graph_builder: FocusGraphBuilder,
    response_builder: FocusResponseBuilder,
}

impl FocusRuntime {
    pub fn prepare(&self, intent: QueryIntent) -> Result<FocusPreparedResult> {
        // 1. Detect index mode via Store::read_index_mode() + is_rich_index_mode().
        //    Do NOT use project_metadata key existence; generation keys are seeded
        //    even for empty DBs.
        // 2. If rich/full index is available, execute the DB-backed query path.
        // 3. Otherwise run focus path:
        //    bootstrap minimum → locate seed → build closure → scoped resolve
        //    → scoped graph overlay → response contract → background expansion.
        todo!()
    }
}
```

This is the architectural replacement for the old query-time lazy control
plane. The old lazy execution services remain below it.

### 3.1 New types in `atlas-engine/src/focus/`

```rust
// ── file: atlas-engine/src/focus/types.rs ──

/// What the user is looking at.
pub enum FocusSeed {
    Symbol { name: String, kind: Option<SymbolKind>, language: Language },
    Position { file_id: FileId, line: u32, column: u32 },
    Field { struct_sym: SymbolId, field_path: String },
    File { file_id: FileId },
}

/// Internal conversion from existing InvestigationFocus:
impl From<InvestigationFocus> for FocusSeed {
    fn from(f: InvestigationFocus) -> Self {
        match f {
            InvestigationFocus::Symbol(id) => {
                // Look up symbol to extract name/kind/language
                // (requires Store access)
                todo!("need store to convert SymbolId → FocusSeed::Symbol")
            }
            InvestigationFocus::Position { file_id, line, col } => {
                FocusSeed::Position { file_id, line, col }
            }
            InvestigationFocus::Field { struct_sym, field_path } => {
                FocusSeed::Field { struct_sym, field_path }
            }
        }
    }
}

/// Budget + strategy + expiration.
pub struct FocusWindow {
    pub seed: FocusSeed,
    pub strategies: Vec<ClosureStrategy>,
    pub budget: WindowBudget,
    pub language: Language,
    pub max_iterations: u32,
}

pub enum ClosureStrategy {
    ImportNeighborhood { depth: u32 },
    SameDirectory,
    CallGraph { direction: Direction, depth: u32 },
    TypeGraph { max_depth: u32 },
}

pub struct WindowBudget {
    pub max_files: usize,      // 30 default
    pub max_time_ms: u64,      // 18000 default (from LazyBudget)
    pub max_symbols: usize,    // 0 = unlimited
    pub max_edges: usize,      // 0 = unlimited
    pub max_fanout_per_name: usize,  // 20
    pub max_bytes: u64,        // 0 = unlimited
    pub max_iterations: u32,   // 3
}

impl From<&LazyBudget> for WindowBudget {
    fn from(b: &LazyBudget) -> Self {
        WindowBudget {
            max_files: b.files_consumed().max(30),
            max_time_ms: 18_000,
            max_symbols: 0,
            max_edges: 0,
            max_fanout_per_name: 20,
            max_bytes: 0,
            max_iterations: 3,
        }
    }
}
```

### 3.2 Bounded Fixed-Point Engine

`ClosureEngine` owns closure planning and iterative expansion. It migrates the
useful semantics from `LazyCoordinator::ensure_structural_with_closure`
(include closure, job claim, budget, query_id propagation), but it is not a
second extraction engine. File and unit facts are still built by the lazy
execution services.

```rust
// ── file: atlas-engine/src/focus/engine.rs ──

pub struct ClosureEngine {
    store: Arc<Store>,
    lazy_structural: LazyStructuralService,
    lazy_dataflow: LazyDataflowService,
    include_roots: Vec<IncludeRoot>,
    max_iterations: u32,
}

impl ClosureEngine {
    /// Build a focus closure with bounded fixed-point iteration.
    pub fn build_closure(
        &self,
        window: &FocusWindow,
    ) -> Result<FocusClosure> {
        let mut closure = FocusClosure::new(&window.seed);
        let mut iteration = 0;

        loop {
            // 1. Plan: ask strategies what to add next
            let additions = self.plan_additions(&window.strategies, &closure)?;

            if additions.is_empty() { break; } // fixed point

            // 2. Budget check
            if !window.budget.can_absorb(&additions) {
                closure.gaps.push(KnownGap::BudgetExhausted {
                    strategy: format!("iteration {}", iteration),
                    remaining: additions.len(),
                });
                break;
            }

            // 3. Extract: use existing LazyStructuralService through
            //    extraction_jobs claim + extraction_state freshness checks.
            for file_id in &additions {
                // Reuses lazy structural execution; Focus owns the policy.
                let result = self.lazy_structural
                    .ensure_structural_for_file(file_id, &budget)?;
                closure.mark_extracted(file_id, result.precision);
            }

            // 4. Resolve: scoped to current closure
            let resolved = self.resolver
                .resolve_for_closure(&closure.files)?;
            // writes to reference_resolutions table

            // 5. Termination
            iteration += 1;
            if iteration >= window.max_iterations { break; }
        }

        // 6. Commit: atomic visibility switch
        self.commit_closure(&closure)?;

        Ok(closure)
    }

    /// Plan additions based on strategies + current closure state.
    fn plan_additions(
        &self,
        strategies: &[ClosureStrategy],
        closure: &FocusClosure,
    ) -> Result<Vec<FileId>> {
        let mut additions = Vec::new();

        for strategy in strategies {
            match strategy {
                ClosureStrategy::ImportNeighborhood { depth } => {
                    // Expand from current closure files through imports table
                    let new_imports = self.expand_imports(
                        &closure.files, *depth, &closure.visited
                    )?;
                    additions.extend(new_imports);
                }
                ClosureStrategy::SameDirectory => {
                    // Find sibling files in same directories as closure files
                    let siblings = self.find_siblings(&closure.files)?;
                    additions.extend(siblings);
                }
                ClosureStrategy::CallGraph { direction, depth } => {
                    // Expand from current closure symbols through call edges
                    let callee_caller = self.expand_callgraph(
                        &closure.symbols, *direction, *depth
                    )?;
                    additions.extend(callee_caller);
                }
                ClosureStrategy::TypeGraph { max_depth } => {
                    // Expand through type references
                    let type_deps = self.expand_types(
                        &closure.symbols, *max_depth
                    )?;
                    additions.extend(type_deps);
                }
            }
        }

        // Dedup against visited + closure files
        additions.retain(|f| !closure.visited.contains(f));
        additions.sort();
        additions.dedup();

        Ok(additions)
    }
}
```

### 3.3 DB Schema (new tables only, no existing table modified)

```sql
CREATE TABLE closure_generations (
    closure_id TEXT PRIMARY KEY,        -- format: "cl_{hash}"
    committed_generation INTEGER NOT NULL DEFAULT 0,
    state TEXT NOT NULL DEFAULT 'building',  -- building | committed | stale | cancelled
    committed_at TIMESTAMP,
    created_at TIMESTAMP DEFAULT (datetime('now'))
);

CREATE TABLE closure_coverage (
    closure_id TEXT NOT NULL REFERENCES closure_generations(closure_id),
    file_id BLOB NOT NULL,
    source TEXT NOT NULL,               -- 'extracted_structural' | 'extracted_manifest' | 'symbol_hints'
    visibility_state TEXT NOT NULL DEFAULT 'staged',  -- staged | visible | stale
    generation INTEGER NOT NULL,
    content_hash TEXT,
    coverage TEXT NOT NULL,
    confidence TEXT NOT NULL,
    extracted_at TIMESTAMP DEFAULT (datetime('now')),
    PRIMARY KEY (closure_id, file_id, generation)
);

CREATE TABLE reference_resolutions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    reference_id BLOB NOT NULL,         -- FK to references.reference_id
    closure_id TEXT NOT NULL REFERENCES closure_generations(closure_id),
    generation INTEGER NOT NULL,
    resolution_scope TEXT NOT NULL,     -- ClosureComplete | Boundary | ProjectWide
    target_symbol_id BLOB,             -- resolved target (nullable if unresolved)
    coverage_tier TEXT NOT NULL,
    semantic_confidence TEXT NOT NULL,
    resolution_strategy TEXT NOT NULL,  -- ClosureReachable | ClosureImports | ProjectWide | ...
    provenance TEXT,
    is_visible BOOLEAN NOT NULL DEFAULT 0,
    created_at TIMESTAMP DEFAULT (datetime('now'))
);

CREATE INDEX idx_ref_res_cl_vis ON reference_resolutions(closure_id, generation, is_visible);

CREATE TABLE symbol_edge_candidates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source BLOB NOT NULL,
    target BLOB,
    kind TEXT NOT NULL,
    coverage_tier TEXT NOT NULL,
    semantic_confidence TEXT NOT NULL,  -- only Medium | Low
    candidate_count INTEGER,
    closure_id TEXT NOT NULL REFERENCES closure_generations(closure_id),
    generation INTEGER NOT NULL,
    is_visible BOOLEAN NOT NULL DEFAULT 0,
    created_at TIMESTAMP DEFAULT (datetime('now'))
);

CREATE TABLE known_gaps (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    closure_id TEXT NOT NULL REFERENCES closure_generations(closure_id),
    gap_kind TEXT NOT NULL,
    details TEXT NOT NULL,              -- JSON
    created_at TIMESTAMP DEFAULT (datetime('now'))
);
```

---

## 4. Phase 3: FocusScheduler + Background Jobs (2 weeks)

### 4.1 Priority Model

```rust
pub enum FocusPriority {
    Sync,         // User is waiting (MCP tool call)
    UserFocus,    // User's current investigation
    Recent,       // Recently touched files/symbols
    Speculative,  // Background pre-warming
}

pub struct FocusScheduler {
    sync_queue: VecDeque<FocusJob>,
    user_focus_queue: VecDeque<FocusJob>,
    recent_queue: VecDeque<FocusJob>,
    speculative_queue: VecDeque<FocusJob>,
    active_job: Option<FocusJob>,
    writer_coordinator: ProjectWriteCoordinator,
}
```

### 4.2 Writer Arbitration

```rust
pub struct ProjectWriteCoordinator {
    lock: Mutex<()>,
}

impl ProjectWriteCoordinator {
    /// Claim write access. Sync jobs preempt background.
    pub fn claim(&self, priority: FocusPriority) -> WriteGuard {
        match priority {
            FocusPriority::Sync => {
                // Sync always wins. Cancel background if running.
                // Signal cancellation to background job(s).
                WriteGuard::acquire(self)
            }
            FocusPriority::UserFocus => {
                // Preempts Recent + Speculative.
                WriteGuard::acquire_if_idle_or_cancellable(self)
            }
            _ => {
                // Queue. Don't interrupt active jobs.
                WriteGuard::acquire_non_blocking(self)
            }
        }
    }

    /// Exclusive mode: full index. No focus jobs allowed.
    pub fn enter_exclusive(&self) -> ExclusiveGuard { /* ... */ }
}
```

### 4.3 Job Lifecycle

```rust
enum FocusJobState {
    Planned,
    Extracting { phase: String, files_done: usize, files_total: usize },
    Resolving { resolved: usize, total: usize },
    GraphBuilding { edges_built: usize },
    Committed { generation: u64 },
    Stale { since: u64 },  // fingerprint generation
    Cancelled,
    Failed(String),
}
```

### 4.4 Background Pre-warming

```rust
impl FocusScheduler {
    /// Speculatively warm the user's current investigation.
    pub fn prewarm_investigation(
        &self,
        investigation: &Investigation,
    ) {
        // Convert Investigation → FocusWindow
        let window = FocusWindow::from_investigation(investigation);

        // Queue as Speculative priority
        self.enqueue(window, FocusPriority::Speculative);
    }

    /// Predictive expansion: when user reads a file, pre-warm its imports.
    pub fn on_file_read(&self, file_id: FileId) {
        let seed = FocusSeed::File { file_id };
        let window = FocusWindow {
            seed,
            strategies: vec![
                ClosureStrategy::ImportNeighborhood { depth: 1 },
                ClosureStrategy::SameDirectory,
            ],
            budget: WindowBudget::background(),
            language: self.guess_language(file_id),
            max_iterations: 1,
        };
        self.enqueue(window, FocusPriority::Recent);
    }
}
```

---

## 5. Phase 4: MCP Response Contract + Gaps (1 week)

### 5.1 Response Envelope (all MCP tools)

The MCP envelope exposes analysis quality and background work state, not the
focus implementation. Public responses must not contain `focus`, `closure_id`,
`FocusPriority`, `ClosureComplete`, scheduler queue names, or any other
implementation-specific focus vocabulary.

Every MCP analysis response is also an external cognitive interface for agents:
it must summarize the current analysis state in stable, user-facing terms. The
agent should never infer readiness, completeness, or retry behavior by reading
internal lazy/focus/job fields.

```json
{
  "result": { "...": "tool-specific" },

  "analysis": {
    "state": "usable_partial",
    "scope": "local",
    "summary": "Local structural facts are available; boundary references are still being refined.",
    "next_action": "use_result_or_wait_for_refinement"
  },

  "precision": {
    "coverage": "local_complete",
    "confidence": "high"
  },

  "coverage_counts": {
    "local_complete": 8,
    "boundary": 12,
    "basic": 45
  },

  "gaps": [
    {
      "kind": "IndirectCall",
      "callsite": "io.rs:145",
      "reason": "trait object dispatch"
    },
    {
      "kind": "BudgetExhausted",
      "strategy": "ImportNeighborhood",
      "remaining": 15
    }
  ],

  "work": {
    "relevant": true,
    "status": "running",
    "items": [
      {
        "id": "task-00012",
        "kind": "analysis_refinement",
        "state": "running",
        "scope": "local",
        "reason": "building more local reference and call graph evidence",
        "progress": { "percent": 68 },
        "waitable": true,
        "retry_after_ms": 1000
      }
    ]
  }
}
```

`analysis` is the canonical public interpretation of all internal analysis
state:

| Field | Meaning |
|-------|---------|
| `state` | One of `ready`, `usable_partial`, `building`, `blocked`, `failed`, `stale`. |
| `scope` | One of `repo`, `local`, `file`, `symbol`, `query`, `corpus`. |
| `summary` | Short, user/agent-readable explanation of what is currently known. |
| `next_action` | One of `use_result`, `use_result_or_wait_for_refinement`, `wait`, `narrow_scope`, `run_full_index`, `retry`, `inspect_gaps`. |

Internal states from `extraction_state`, `extraction_jobs`, focus closures,
bootstrap tiers, resolver coverage, graph refresh, query snapshots, and corpus
version work must be normalized into this `analysis` view plus `precision`,
`coverage_counts`, `gaps`, and `work`. They must not be exposed as independent
public state machines.

`coverage_counts` uses public coverage labels only:

| Public label | Meaning |
|--------------|---------|
| `repo_complete` | Full-index facts cover the repository-wide query. |
| `local_complete` | The current local analysis scope is structurally complete. |
| `boundary` | Results are complete inside the local scope but stop at declared boundaries. |
| `partial` | Some requested evidence is missing or budget-limited. |
| `basic` | Manifest/name-level facts only. |

`work` is the only public background-work model, but ordinary tool responses
include it only when background work is relevant to the current result. A tool
response must not include unrelated global indexing, project activation, corpus
sync, speculative prewarm, or scheduler queue state. Global work state is
available only through explicit status/task tools.

Include `work` when:

- the current query triggered background refinement;
- the current result is partial/building/stale and background work can change
  its quality or availability;
- `analysis.next_action` is `wait` or `use_result_or_wait_for_refinement`;
- the response needs to expose a waitable public task/work id.

`work.items[].id` is a public task/work id; it must not be a closure id,
extraction job row id, scheduler generation, or internal queue key. If
`waitable=true`, the id must be accepted by the public task waiting API. If a
background item is advisory only, expose `waitable=false` and a
`retry_after_ms` hint instead.

### 5.2 Integration Points (existing MCP tools)

Each existing MCP tool returns the unified response envelope. Example for
`atlas_calls`:

```
{
          "callers": [...], "callees": [...],
          "analysis": {
            "state": "ready",
            "scope": "local",
            "summary": "Local call graph evidence is available.",
            "next_action": "use_result"
          },
          "precision": { "coverage": "local_complete", "confidence": "high" },
          "coverage_counts": { "local_complete": 5, "boundary": 2, "basic": 0 },
          "gaps": []
}
```

### 5.3 Known Gap Reporting

When `ClosureEngine` hits a gap during expansion, it records it in `known_gaps` table:

```rust
// Examples of gap creation during planning/expansion:
closure.record_gap(KnownGap::UnresolvedImport {
    from: "kernel/sched/core.c",
    import_path: "<linux/config.h>",
});
closure.record_gap(KnownGap::HighFanoutName {
    name: "printk",
    candidates: 1420,
});
closure.record_gap(KnownGap::BudgetExhausted {
    strategy: "CallGraph(depth=2)",
    remaining: 47,
});

// MCP response includes active gaps for the query scope:
let active_gaps = store.get_active_gaps(&closure_id, &query_symbols);
```

---

## 6. Phase 5: Visibility-Aware ClosureReachableSymbols (1 week)

### 6.1 Language Visibility Filters

```rust
// in atlas-engine/src/focus/visibility_filter.rs

pub trait VisibilityFilter: Send + Sync {
    fn is_visible(&self, symbol: &SymbolDef, from_file: FileId, context: &VisibilityContext) -> bool;
}

struct CVisibilityFilter;
impl VisibilityFilter for CVisibilityFilter {
    fn is_visible(&self, symbol: &SymbolDef, _from: FileId, _ctx: &VisibilityContext) -> bool {
        // Exclude static functions/variables
        !matches!(symbol.visibility, Visibility::Static)
            && !matches!(symbol.visibility, Visibility::StaticInline)
    }
}

struct RustVisibilityFilter;
impl VisibilityFilter for RustVisibilityFilter {
    fn is_visible(&self, symbol: &SymbolDef, from_file: FileId, ctx: &VisibilityContext) -> bool {
        match symbol.visibility {
            Visibility::Public => true,
            Visibility::PubCrate => ctx.same_crate(from_file, &symbol.file_id),
            Visibility::PubSuper => ctx.same_module_parent(from_file, &symbol.file_id),
            Visibility::Private => false,
        }
    }
}
```

### 6.2 Resolution Order (from v3.1, now with visibility filters)

```
1. Builtins           compiler intrinsics
2. LexicalScope       local variables, parameters, nested scopes
3. Container          class/module/struct containing the reference
4. SameFile           other symbols in same source file
5. Imports            direct imports/includes of current file
6. ClosureReachable   closure symbols filtered by language visibility + reachability
7. ClosureImports     import closure of ALL closure files (weaker than tier 6)
8. ProjectWide        DB-wide manifest fallback (response-only unless Certain)
```

Tier 6 (`ClosureReachable`) applies `VisibilityFilter` per language. Filters are registered by language at engine init.

---

## 7. Phase 6: Edge Conflict + Durable Policy (1 week)

### 7.1 Conflict Rules

```
Full-index/global edges are immutable unless the full-index pipeline rebuilds them.
Focus edges are closure-scoped and never overwrite global graph state.
Higher coverage wins inside the same closure scope.
Same coverage, higher confidence wins inside the same closure scope.
Medium/Low confidence → scoped candidate table, not global symbol_edges.
High fanout names → KnownGap, no edge.
```

### 7.2 Focus Persistence Policy

Focus graph results are scoped to `closure_id + generation`. They are durable
inside that scope, not globally durable. Only the full-index pipeline writes
repo-wide canonical `symbol_edges`.

| Confidence | Persistence |
|-----------|-------------|
| Certain | scoped focus edge overlay |
| High | scoped focus edge overlay |
| Medium | scoped candidate edge |
| Low | Not persisted, returned in gaps |
| High fanout | KnownGap |

Implementation may start with `symbol_edge_candidates` as the overlay table if
it carries `closure_id`, `generation`, coverage, and confidence. If canonical
closure-scoped edges need separate query semantics, introduce a dedicated
`closure_symbol_edges` table rather than writing global `symbol_edges`.

---

## 8. CLI

```
atlas index --full            # Explicit full index (unchanged)
```

### 8.1 Transparency Constraint

The focus mechanism is **internal infrastructure — zero user-facing surface**.

- **No `atlas focus` command.** Users never pre-warm closures, check coverage, or
  list gaps manually.
- **No `atlas open` daemon.** Focus is an on-demand capability of the existing
  `atlas-mcp` process, not a separate runtime.
- **Activation is silent.** When an MCP query targets a project with no full
  index (or incomplete coverage), the focus engine activates automatically in
  background threads. The user sees only improved response quality over time,
  never a "focus mode" indicator.
- **No default `focus` response/status section.** MCP tools and `atlas_status`
  may expose analysis quality (`precision`, `coverage_counts`, `gaps`) and
  background work (`work` / task APIs), but must not expose focus bootstrap
  tiers, closure ids, scheduler priorities, closure counts, or focus-specific
  pending queues as default public fields.
- **`atlas index --full` remains the explicit path.** For repos where full
  indexing is feasible, the user invokes it once and focus is never needed.
  Focus fills the gap for repos where full indexing is impractical.

---

## 9. Implementation Sequence

```
Phase 0 ── Precision bridge + schema safety       (Week 1)
Phase 1 ── BootstrapManager                       (Week 2-3)
           file_inventory, fingerprints, SymbolHints
Phase 2 ── FocusRuntime + QueryIntent             (Week 3-4)
           single query-time control-plane entry
Phase 3 ── ClosureEngine                          (Week 4-6)
           bounded fixed-point over lazy execution engines
Phase 4 ── ScopedResolver + FocusGraphBuilder     (Week 6-8)
           reference_resolutions + scoped graph overlay
Phase 5 ── MCP integration, one tool first         (Week 8-9)
           calls → FocusRuntime::prepare
Phase 6 ── Migrate remaining query tools          (Week 9-11)
           explore/search/context/trace/lifecycle/branch_diff/resume
Phase 7 ── Remove old lazy control-plane entries   (Week 11-12)
           LazyOrchestrator, direct MCP ensure_* helpers,
           LazyCoordinator after responsibilities have moved
```

---

## 10. What Doesn't Change

| Component | Status |
|-----------|--------|
| `LazyStructuralService` | Stays as an execution engine. FocusRuntime/ClosureEngine decides when to call it. |
| `LazyDataflowService` | Stays as an execution engine. FocusRuntime decides when dataflow is required for the query intent. |
| `CandidateProvider` trait | Stays. Used for Tier 1 SymbolHints bootstrap. |
| `extraction_state` table | Stays. Freshness and capability masks remain source-of-truth. |
| `extraction_jobs` table | Stays. Focus jobs must use the same in-flight dedup and pending observability boundary. |
| `symbols`, `references`, `symbol_edges` tables | Keep their global semantics. Focus writes scoped results to overlay tables and never mutates `references.resolved_*`. |
| `ExtractionMode` enum | Untouched. Focus requests use existing modes. |
| `tree-sitter` frontends | Untouched. 14 languages. |
| `filesync` pipeline | Untouched. `atlas index --full` unchanged. |
| All 14 language adapters | Untouched. |

## 10.1 What Must Be Removed Before Completion

These boundaries are temporary scaffolding only. The target architecture does
keeps only the target MCP contract:

| Component | Removal condition |
|-----------|-------------------|
| `LazyOrchestrator` | Remove once `FocusRuntime` owns query policy, budget, diagnostics, and background prewarm. |
| `LazyCoordinator` | Remove or make private once job claim, closure planning, query_id propagation, and prewarm are migrated. |
| MCP `ensure_structural_for_files/name` helpers | Remove after all MCP query tools route through `FocusRuntime::prepare(QueryIntent)`. |
| Focus-specific public pending/status fields | Replace with the unified `work` envelope and task APIs. |
| Old public response fields such as `precision_tier`, `pending_closures`, `pending_job_ids`, `lazy_diagnostics` | Delete from the MCP contract; do not preserve aliases. |

---

## 11. Risk + Mitigation

| Risk | Mitigation |
|------|------------|
| FocusRuntime bypasses lazy cache/job semantics | FocusRuntime must call the lazy execution layer through `extraction_state` and `extraction_jobs`; no ad-hoc extraction path. |
| Double control plane persists | Treat `LazyOrchestrator`, MCP `ensure_*`, and `LazyCoordinator` as temporary old-control-plane scaffolding with explicit removal conditions. |
| Scoped resolution pollutes global DB state | Focus path writes `reference_resolutions` and scoped graph overlay only; full index remains the only path that updates global `references.resolved_*`. |
| Background jobs corrupt DB state | `closure_coverage.visibility_state='staged'` until `Committed`. MCP reads only `visible`. |
| SymbolHints missing gives false "not found" | Hint-not-truth semantics. MCP response distinguishes "not in hints" from "not found." |
| Fixed-point explodes (Linux headers) | `max_iterations=3`, budget per-iteration. C header depth limited by `ImportNeighborhood.depth`. |
| Existing MCP tools break while being rewired | Move one intent at a time through `FocusRuntime`; update each tool directly to the unified analysis envelope before considering it complete. |
| Write contention (sync vs background) | `ProjectWriteCoordinator` with Sync-preempt. Checkpoint at file boundary. |
