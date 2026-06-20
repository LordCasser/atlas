# Focus Architecture Notes

## Hot Region Control Plane

Focus mode is not a stateless per-query parser. The runtime owns a background
control plane that keeps hot regions around the user's current investigation and
uses those regions to decide what should be analyzed next.

Foreground tool calls must build only a bounded minimal closure and return
partial precision when necessary. They should also enqueue background work so a
follow-up call can see better local coverage without forcing the first call to
wait for a project-wide scan.

Hot regions are hierarchical:

- Seed level: the exact file, symbol, or source position the user queried.
- Local level: the synchronous closure built inside the foreground budget.
- Boundary level: files reached by the local closure and likely to be touched by
  the next query.
- Expanded level: background work grown from a boundary hit.

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
