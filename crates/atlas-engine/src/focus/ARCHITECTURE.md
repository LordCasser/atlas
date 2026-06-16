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

Scheduler queues execute work, but they do not decide region strategy. Region
state, boundary detection, and expansion policy belong in `FocusRuntime` so MCP
tools get a single control-plane entry point.
