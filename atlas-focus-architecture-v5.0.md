# Atlas Focus-driven Incremental Analysis — Implementation Plan v5.0

> **Scope**: Atlas only. Corpus compatibility confirmed via `SourceUniverse` trait (see v4.0).
> **Strategy**: Evolve existing lazy infrastructure. Don't rewrite.
> **Constraint**: Focus is internal infrastructure. Zero user-facing surface.
> No CLI commands, no manual pre-warm, no coverage dashboard. Activation is
> silent and automatic when the project has no full index.

---

## 0. What Changes vs What Stays

| Existing | Action |
|----------|--------|
| `InvestigationFocus` (Symbol/Field/Position) | **Upgrade** → `FocusSeed` (add `File` variant + `language` field) |
| `Investigation` | **Upgrade** → `FocusWindow` (add budget + strategies + max_iterations) |
| `ClosurePlanner` | **Extend** → `ImportNeighborhood` strategy in the closure strategy catalog |
| `LazyBudget` | **Extend** → `WindowBudget` (add symbol/edge/fanout/bytes limits) |
| `LazyCoordinator` | **Upgrade** → `ClosureEngine` (bounded fixed-point) |
| `PrecisionTier` (6-tier enum) | **Bridge** → `Precision { coverage, confidence }` with migration adapter |
| `LazyStructuralService` | **Keep**, now called by ClosureEngine |
| `LazyDataflowService` | **Keep**, now called by ClosureEngine |
| `CandidateProvider` trait | **Keep**, becomes Tier 1 `SymbolHints` implementation |
| `extraction_jobs` table | **Extend** → add `closure_id`, `generation` columns |
| `symbols`, `references`, `symbol_edges` tables | **Untouched** |
| `extraction_state` table | **Untouched** |
| `files` table | **Untouched** |

---

## 1. Phase 0: Precision Migration Bridge (1 week)

**Don't break existing MCP tools.** Add adapter, then migrate.

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

### MCP tools: return both fields

```rust
struct McpResponse {
    precision_tier: PrecisionTier,  // keep for old clients
    precision: Precision,            // new for focus-aware clients
    // ...
}
```

Remove `precision_tier` in Phase 4.

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

Filled on `atlas open` by reusing existing `filesync::phase_discover` output. Cost: a single `readdir` walk. For 60K files on modern SSD: ~0.3s.

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

## 3. Phase 2: Focus Primitives + ClosureEngine (3 weeks)

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

/// Migration from existing InvestigationFocus:
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

Upgrades `LazyCoordinator::ensure_structural_with_closure` from single-pass BFS to iterative expansion.

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

            // 3. Extract: use existing LazyStructuralService
            for file_id in &additions {
                // Reuses existing ensure_structural_with_tracking
                let result = self.lazy_structural
                    .ensure_structural_for_files(&[file_id], &budget)?;
                closure.mark_extracted(file_id, result.precision_tier);
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
    precision_tier TEXT NOT NULL,
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

```json
{
  "result": { "...": "tool-specific" },

  "precision": {
    "coverage": "ClosureComplete",
    "confidence": "Certain"
  },

  "coverage_counts": {
    "ClosureComplete": 8,
    "Boundary": 12,
    "Manifest": 45
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

  "pending": [
    {
      "closure_id": "cl_a1b2c3d4",
      "state": "Resolving",
      "percent": 68,
      "eta_ms": 2100
    }
  ]
}
```

### 5.2 Integration Points (existing MCP tools)

Each existing MCP tool adds the response envelope. Example for `atlas_calls`:

```
Before: { "callers": [...], "callees": [...], "precision_tier": "Exact" }
After:  {
          "callers": [...], "callees": [...],
          "precision_tier": "Exact",       // backward compat
          "precision": { "coverage": "ClosureComplete", "confidence": "Certain" },
          "coverage_counts": { "ClosureComplete": 5, "Boundary": 2, "Manifest": 0 },
          "gaps": [],
          "pending": []
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
Certain edges are immutable — never overwritten.
Higher coverage wins.
Same coverage, higher confidence wins.
Medium/Low confidence → symbol_edge_candidates table, not symbol_edges.
High fanout names → KnownGap, no edge.
```

### 7.2 Durable vs Response-Only (from v3.1 Section 4.11, unchanged)

| Confidence | Persistence |
|-----------|-------------|
| Certain | `symbol_edges` |
| High | `symbol_edges` |
| Medium | `symbol_edge_candidates` (response-only) |
| Low | Not persisted, returned in gaps |
| High fanout | KnownGap |

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
- **`atlas index --full` remains the explicit path.** For repos where full
  indexing is feasible, the user invokes it once and focus is never needed.
  Focus fills the gap for repos where full indexing is impractical.

---

## 9. Implementation Sequence

```
Phase 0 ── Precision bridge + compat          (Week 1)
Phase 1 ── Bootstrap: file_inventory,          (Week 2-3)
           content fingerprints, SymbolHints
Phase 2 ── Focus types + ClosureEngine         (Week 3-6)
           + DB schema (new tables only)
Phase 3 ── FocusScheduler + background jobs    (Week 6-8)
Phase 4 ── MCP response contract + gaps        (Week 8-9)
Phase 5 ── Visibility filters                  (Week 9-10)
Phase 6 ── Edge conflict policy                (Week 10-11)
```

---

## 10. What Doesn't Change

| Component | Status |
|-----------|--------|
| `LazyStructuralService` | Stays. Called by ClosureEngine as extract backend. |
| `LazyDataflowService` | Stays. Called by ClosureEngine for dataflow after structural. |
| `CandidateProvider` trait | Stays. Used for Tier 1 SymbolHints bootstrap. |
| `extraction_state` table | Stays. ClosureEngine checks it for cache decisions. |
| `symbols`, `references`, `symbol_edges` tables | Untouched. Focus writes to overlay tables. |
| `ExtractionMode` enum | Untouched. Focus requests use existing modes. |
| `tree-sitter` frontends | Untouched. 14 languages. |
| `filesync` pipeline | Untouched. `atlas index --full` unchanged. |
| All 14 language adapters | Untouched. |

---

## 11. Risk + Mitigation

| Risk | Mitigation |
|------|------------|
| ClosureEngine breaks existing lazy coordinator | New code in `atlas-engine/src/focus/`. Old `LazyCoordinator` stays. Feature-gate the new path. |
| Background jobs corrupt DB state | `closure_coverage.visibility_state='staged'` until `Committed`. MCP reads only `visible`. |
| SymbolHints missing gives false "not found" | Hint-not-truth semantics. MCP response distinguishes "not in hints" from "not found." |
| Fixed-point explodes (Linux headers) | `max_iterations=3`, budget per-iteration. C header depth limited by `ImportNeighborhood.depth`. |
| Existing MCP tools break | PrecisionTier adapter in Phase 0. Both old and new fields until Phase 4. |
| Write contention (sync vs background) | `ProjectWriteCoordinator` with Sync-preempt. Checkpoint at file boundary. |
