# Focus Architecture Notes

## Hot Region Control Plane

Focus mode is not a stateless per-query parser. The runtime owns a background
control plane that keeps hot regions around the user's current investigation and
uses those regions to decide what should be analyzed next.

Foreground preparation builds a bounded minimal closure and enqueues tracked
refinement. MCP gives that tracked work one 18-second interactive window. If the
required fact layer is still unavailable at the deadline, the public response
contains only a resumable query ticket and the pending reason; provisional
query data is never published. The same background job continues after the
response and `resume_query` replays the original query snapshot.

Query strength is a control-plane input, represented once by `QueryNeed`:

- `Manifest`: symbol inventory and basic metadata.
- `Structural`: exact source/file structure.
- `CallGraph`: resolved cross-file topology.
- `Dataflow`: CFG/dataflow for the query dependency region.

Pre-indexing is a persistent starting point, not a competing runtime mode.
Focus reuses fresh manifest/structural facts and grows the hot region from
them. It skips the control plane only when a finalized whole-project Index
already satisfies the current `QueryNeed`. That decision requires the recorded
manual pipeline grade, an empty include/exclude scope, and a still-sufficient
fresh catalog. Focus enrichment may improve current facts but cannot promote
the authority of the finalized manual Index. Scoped indexes, mixed partial
catalogs, manifest indexes enriched by Focus, and structural caches serving
dataflow queries remain on Focus. Graph provenance uses the same CallGraph
eligibility rule instead of inferring repository authority from rich rows alone.

`TraceVariable`, semantic impact, lifecycle, and branch diff use the existing
Sync-priority Focus closure so the job remains pending through dataflow
materialization. Point trace remains structural; caller/forward trace use the
cross-file call-graph profile.

Hot regions are hierarchical:

- Seed level: the exact file, symbol, or source position the user queried.
- Local level: the synchronous closure built inside the foreground budget.
- Boundary level: files reached by the local closure and likely to be touched by
  the next query.
- Expanded level: background work grown from a boundary hit.

The expanded region is derived from the query dependency shape, not merely the
seed directory. Cross-file trace adds bidirectional call and type frontiers plus
imports/siblings, uses up to five closure iterations, and retains the existing
100-file background cap. Dataflow windows use the same 100-unit ceiling so the
semantic materialization radius does not collapse below the structural hot
region.

Sync dataflow stays centered on the query seed: position seeds use
`ensure_for_position_with_depth`, callable seeds use
`ensure_for_function_with_depth`, and their LazyWindow expansion follows
cross-file caller/callee dependencies. The existing `FocusWindow.max_iterations`
is the single radius input: lifecycle/branch analysis stays function-local
(depth 0), semantic impact follows its requested depth (1–5), and variable trace
uses its requested depth clamped to 2–5. It does not blindly materialize every
function in every closure file.

When a new query lands on the boundary of an existing hot region, the runtime
should expand that region instead of treating it as an unrelated cold request.
Expansion is always queued as background work unless an existing foreground
budget explicitly covers it.

Only refinement jobs returned in `FocusResult.pending_closure_ids` may be
queued by a query. Files already covered by the foreground closure must not be
fanned out into untracked per-file prewarm closures. Closure-scoped resolution,
coverage, and candidate-edge rows are transient materialization facts; the
store clears previous-session control-plane rows when MCP activates a project,
then retains the newest 16 committed closures within the active session. Older
rows are removed after their canonical graph edges have been written.

Scheduler queues execute work, but they do not decide region strategy. Region
state, boundary detection, and expansion policy belong in `FocusRuntime` so MCP
tools get a single control-plane entry point.

Bootstrap may populate an empty store, but it must not start project-wide work
when persistent file inventory or source facts already exist. Closure resolution
uses indexed exact-name lookup for its local fallback; it must not build the
project-wide in-memory symbol index. Import scope, source path classification,
and proximity roots are computed once per source file, not once per reference.

## Store Boundary

MCP project open uses one project-local persistent SQLite store at
`project/.atlas/atlas.db`. Atlas does not build an application-level
memory-store plus persistent-store fallback layer for MCP queries. SQLite owns
the physical cache hierarchy through its page cache, mmap, and WAL behavior.

FocusRuntime owns semantic locality only: hot regions, bounded foreground
closures, background expansion, and eviction priority for analysis work. LRU can
reprioritize or evict hot-region metadata, but it must not be used as a second
source of truth for indexed facts. Query tools should read and write through the
active project store. MCP responses expose pending refinement through
`analysis.retry_after_ms` and terminal limitations through `gaps`; internal
precision and physical cache state are not part of the public query contract.
