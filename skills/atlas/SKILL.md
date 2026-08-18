---
name: atlas
description: >-
  MCP-first deterministic local code-facts engine (tree-sitter → SQLite) for
  scoped symbol search, callers/callees, paths, impact, variable/call tracing,
  and C/C++ lifecycle via Focus-driven on-demand extraction. Use for structure,
  data/call flow, change impact, or agent context on a local checkout—prefer
  Atlas over filename guesses or blind text search. Always project(open) then
  scoped MCP tools; do not run atlas index from the agent (blocks large repos).
license: MIT
metadata:
  version: "1.6.1"
  repository: https://github.com/lordcasser/atlas
---

# Atlas (MCP-first)

Use Atlas as the **deterministic code-facts layer** before reasoning about a repository.
Prefer Atlas facts over guessing from filenames or text search.

**This skill is for the MCP tool surface.** On large codebases, full-repo
`atlas index` can take a long time and starve the session. **Agents must not
start a full CLI index.** **Focus** (sole query-time product path) already
materializes **local structural + dataflow** for the query neighborhood (seed
closure + on-demand unit dataflow under Focus materialize). Stay inside MCP:
open → narrow tools → `resume_query` when asked.

## When to use

- “What calls X?” / “Who depends on this file?” / “Where does this value come from?”
- Impact of changing a symbol; exploring an unfamiliar module
- C/C++ field/resource lifecycle or branch side effects
- Building **bounded** agent context from real facts (module/file/function scope)

Do **not** treat Atlas as a compiler or LSP. Always surface capability limits.

## Hard rules for agents

1. **Only MCP tools** from this skill’s workflow. Do **not** shell out to
   `atlas index`, `atlas index --analysis full`, or long `atlas sync` of the
   whole tree as part of normal investigation.
2. Always **`project(action="open", project_path=...)`** before other tools.
   The server starts unopened; cwd alone is not an open project.
3. Prefer **scoped** work: `search` always needs `scope`; start from a file/symbol
   you care about; use `resume_query` instead of “index everything.”
4. On large repos, **local Focus facts are enough** for most questions:
   structural edges in the seed/closure, and **on-demand dataflow/CFG** for
   `trace(kind="variable")`, `lifecycle`, `branch_diff` in that region.
5. Never block the user waiting for a project-wide index.

Full-repo CLI indexing is an **optional human/operator** action (see
[references/tool-guide.md](references/tool-guide.md)), not an agent step.

## Requirements

- Atlas MCP available (`atlas mcp` or host-configured server)
- Local project checkout
- Open creates/opens `project/.atlas/atlas.db`; **scoped MCP queries populate facts**
  (Focus structural + on-demand dataflow). No pre-built full index required.

## Language support (summary)

14 languages build by default. All 14 expose limited function/method CFG, but
construct coverage and confidence differ by language; overall
**DataflowInterproc** still does **not** mean every feature works everywhere. Check:

- `project(action="status")` (verbose if needed)
- Trace response `capability` / diagnostics

Treat `scope_aware_binding` as an evidence-backed precision marker, not compiler
equivalence. Read the profile limitations for destructuring, pattern,
callback, definite-assignment, and dynamic-namespace boundaries before relying
on an apparently resolved same-name use.

All 14 persisted language identities preserve aggregate read-modify-write provenance
for their supported direct-variable mutation forms; each response still reports its
own language identity. Cangjie additionally preserves direct simple reassignment and
direct-identifier non-conditional compound/postfix update forms.
Attribute/member/field/navigation、subscript/element/array/index、receiver and
pointer/dereference mutation targets remain conservative. TypeScript、JavaScript and
ArkTS direct-identifier `&&=`/`||=`/`??=` preserve path-insensitive old-value/RHS
may-provenance through Read 0.75 and Assign 0.90, but do not prove RHS execution or
operator-specific truthiness/nullish control dependency. Ruby and Cangjie
`||=`/`&&=` remain conditional-write boundaries. Overloaded/dynamic operator
semantics、numeric promotion/boxing and prefix/postfix result timing remain
conservative where the language supports them.

TypeScript、JavaScript and ArkTS `let`/`const` declaration destructuring binds
simple、renamed、nested、default-left and rest targets in the enclosing block.
The whole initializer reaches every supported target through aggregate Assign
0.85；computed keys and default RHS expressions remain reads. Treat this as
aggregate origin evidence, not exact property/index projection. `var`
declarations and assignment destructuring remain conservative.

TypeScript、JavaScript and ArkTS function、method and arrow parameter
destructuring binds simple、renamed、nested、default-left and rest leaves in
the function scope. Leaves from the same top-level parameter retain the same
runtime argument position, excluding TypeScript's erased `this` parameter. The
whole call argument reaches each leaf through aggregate `ArgToParam`, whether
supplied by a Full Index summary or a Focus runtime edge. Computed keys and
default RHS expressions remain reads. Treat this as aggregate origin evidence,
not exact property/index projection or proof that a parameter default activates.

TypeScript、JavaScript and ArkTS `let`/`const` `for...of`/`for...in` simple or
nested pattern captures are loop-scoped；direct existing-local assignment forms
reuse the prior binding. The whole iterable/object reaches each supported target
through aggregate Assign 0.65, including value provenance for `for await`. Treat
that edge as a possible aggregate origin, not exact element/key projection or
proof of async scheduling. `var` function-scoped loop binding semantics and
member/subscript iteration targets remain conservative.

Rust ordinary `let`/`let-else` captures join the enclosing block and activate
only after the complete declaration. Initializers and `let-else` alternatives
therefore keep source-earlier same-name identities；uses after the declaration
resolve to the new binding. Direct identifiers receive whole-value Assign 0.90.
Ordinary let initializers、`match` scrutinees and source-ordered match-guard、
`if` and `while` let-condition RHS values preserve exact syntactic access paths
for fixed tuple、tuple-struct、struct and slice-prefix captures. Variable Trace
can traverse `FieldLoad` 0.80 into an anonymous projection Expr and `Assign`
0.90 into the scoped capture. `if let`/let-chain captures activate in source
order through later conditions and the success block, but do not enter `else`；
`while let` captures remain loop-scoped. Whole-pattern bare/ref/`@` captures and
targets after `..` remain aggregate Assign 0.75；runtime-length suffix projection、
borrow/move mode、compiler irrefutability/type validation and condition/guard
control dependency remain explicit limitations.

### Source encoding and positions

- Atlas decodes project sources internally before extraction. UTF-8 is direct;
  legacy GBK/GB18030 and windows-1252 sources are supported on a best-effort basis.
- Pass project paths and source positions normally; do not transcode or rewrite files
  before querying Atlas.
- Returned source and ranges refer to the decoded UTF-8 view. For a legacy-encoded
  file, byte offsets and columns are not raw-file edit coordinates; Atlas does not
  provide a decoded-to-raw edit mapping.

## Workflow (MCP only)

### 1. Open and status

```
project(action="open", project_path=<path>)
project(action="status")   # optional; understand coverage without indexing
```

Missing DB is created on open. Do **not** “fix empty status” by launching index.

### 2. Cold / Focus protocol (default on large projects)

Without a pre-existing full CLI cache, tools use **Focus** only (not a separate
“lazy product”):

- Foreground builds a **bounded seed closure** (not the whole repo).
- Import/include peers may stay at lightweight `resolution_symbols` until needed.
- **Dataflow/CFG** is built on demand for units in trace/lifecycle-style work
  (Focus internal materialize).
- Background refinement may continue after the first response.
- MCP uses one 18-second interactive window. If required facts finish in time,
  it transparently returns the complete replayed result; otherwise it returns
  only an in-progress ticket and withholds provisional query data.

**Every query:**

1. Call the narrowest MCP tool.
2. Read **`query_id`** and outer **`analysis`**:
   - **`status=failed`** → no result is usable; inspect `pending.detail` and
     re-run the original tool call to retry materialization.
   - **`analysis.retry_after_ms` set** → **not terminal**. Wait (or `tasks`), then
     **`resume_query(query_id=...)`**. This response is a ticket, not usable
     analysis data. Repeat until retry is gone.
   - No retry + **`gaps`** → terminal with limits; report them.
   - No retry + no gaps → complete **for that Focus/local scope**.
3. **Never** treat empty Focus `callers` / thin `callees` as “no callers in the repo.”
   Honor `note` / `gaps` about closure scope.
4. If evidence is still thin: tighten `scope` / symbol selector / `include_roots`,
   deepen carefully, or **resume**—do **not** start `atlas index`.

### 3. Pick the narrowest MCP tool

| Goal | Tool |
|------|------|
| Find by name | `search` (**requires `scope`**, e.g. `"src"`) → `symbol` |
| Callers / callees | `calls(direction="incoming"\|"outgoing")` — fixed one hop; `depth` is ignored |
| Multi-hop | `calls(direction="both", depth=...)` only after one hop + resume if needed |
| File imports | `file_dependencies` |
| Position context | `trace(kind="point")` |
| **Local value origin (dataflow)** | `trace(kind="variable")` — Focus materialize DF for that region |
| Caller / forward chains | `trace(kind="callers"\|"forward")` |
| Path / impact | `path`, `impact` (local/closure-quality unless full cache exists) |
| Dossier / usages | `explore`; `symbol(view="context"\|"usages")` |
| C/C++ lifecycle / branches | `lifecycle`, `branch_diff` (local CFG/DF on demand) |
| Wait / refine | `tasks`, **`resume_query`** |

### 4. Response envelopes (do not mix)

**Non-trace tools:** outer `query_id`, `analysis` (`scope`, `summary`, `basis`,
optional `retry_after_ms`), optional terminal `gaps`, `warnings` / `note`.  
When retry is present, only the ticket and pending reason are published; no
result collection is usable yet.

**Trace tools:** terminal responses contain inner `ok`, `kind`, `capability`,
**`partial_result`**, `diagnostics`, optional `lazy_summary` (mechanism field
name), and `result`. While tracked Focus work is pending, the whole inner trace
body is withheld and only the outer ticket is returned.

### 5. After code edits (still MCP-first)

- Re-run the same MCP tools / `resume_query`; Focus re-extracts dirty seed files as needed.
- Do **not** run whole-tree `atlas sync`/`index` from the agent. If the user already
  maintains a CLI cache, they manage it outside this skill.

## MCP tools (15)

Native short names (hosts may prefix `atlas_`). Install/config:
[references/tool-guide.md](references/tool-guide.md).

| Tool | Required | Notes |
|------|----------|--------|
| `project` | `project_path` on `open` | `open` / `status` / `files` |
| `search` | `query`, **`scope`** | Scope = boundary + focus seed |
| `symbol` | `symbol` or position | `view`: detail / context / usages |
| `calls` | `symbol` | incoming/outgoing=fixed one hop; only `both` honors `depth` |
| `explore` | `symbol` | Dossier |
| `path` | `from`, `to` | Both ends must resolve in available facts |
| `impact` | `symbol` | optional `semantic` (C/C++) |
| `file_dependencies` | `file_path` | manifest=current stored facts, no Focus (terminal gap if incomplete); structural=CallGraph Focus |
| `trace` | per `kind` | Local DF via Focus materialize for `variable` |
| `lifecycle` | `symbol`, `field` | C/C++; function-local |
| `branch_diff` | `symbol` | C/C++ |
| `domain_rules` | — | rule store |
| `fp_dispatches` | — | FP annotations |
| `tasks` | — | optional `query_id` |
| `resume_query` | `query_id` | **primary refinement path** |

Required Focus fact levels are parameter-sensitive: scoped `search` and
`symbol(detail)` need structural facts; `symbol(usages|context)` and
`trace(forward|callers)` need the cross-file call graph;
`file_dependencies(analysis=manifest)` is a pure read while
`analysis=structural` needs CallGraph; `trace(variable)`, semantic impact,
lifecycle, and branch diff need tracked dataflow/CFG materialization.

### Trace kinds

| kind | Need | Default / hard max_depth |
|------|------|--------------------------|
| `point` | file + line + column | — |
| `variable` | file + line + column | 30 / 100 |
| `forward` | `from`, `to` | 10 / 100 |
| `callers` | `symbol` | 20 / 100 |

Collection and traversal arguments are hard-bounded by the handler even when a
client skips JSON Schema validation. Read `returned`, `truncated`, totals, and
per-field truncation metadata before drawing an absence conclusion; narrow the
scope or selector instead of requesting an unbounded response.

## Symbol selector

1. **String** qualified name  
2. **Object** (only `qualified_name` required):

```json
{
  "qualified_name": "turn",
  "file_path": "src/foo.ts",
  "line": 42,
  "kind": "function",
  "language": "typescript"
}
```

Reuse `symbol_ref` from prior results when present.

## Query tactics

- Always pass **`scope`** to `search`.
- Stay local: file/dir scope, exact symbol, then expand.
- Value flow: `trace(point)` then `trace(variable)` — this is how you get **local dataflow**
  without a full index.
- For `op=` / `++` / `--`, place the trace column inside the operator to select
  the aggregate mutation `Expr`; a column on the identifier intentionally
  resolves the higher-priority `Local` node.
- For `calls`, use incoming/outgoing for one hop; deepen only with
  `direction="both"`. Resume before deepening on cold Focus.
- C/C++ system headers: pass `include_roots` when needed.

An operator-built whole-repository Index is reused per query need, not by one
coarse catalog label. For example, a finalized manifest Index remains
authoritative for manifest reads after Focus structurally enriches a few files;
CallGraph/dataflow queries still enter Focus.

## Anti-patterns (do not)

- **`atlas index` / full-tree `atlas sync` from the agent** (especially large repos)
- Tools before `project(action="open")`
- `search` without `scope`
- Treating Focus-empty callers as repo-wide absence
- Skipping `resume_query` when `retry_after_ms` is set
- Claiming full-repo completeness without evidence
- `lifecycle` / `branch_diff` on non-C/C++ without capability check
- Passing removed `storage` on `project(open)`

## Answering rules

- Cite Atlas evidence: names, paths, edge kinds, diagnostics, gaps, notes.
- State Focus/local scope when results are closure-bounded.
- If nothing matches: broaden `search` within MCP, then say no fact matched—**do not index the monorepo**.

## References

- MCP install, host config, operator-only CLI notes: [references/tool-guide.md](references/tool-guide.md)
