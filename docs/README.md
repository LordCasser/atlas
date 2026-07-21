# Atlas documentation

This directory keeps the release-facing documentation for the current Atlas 1.5.x development line. The trace JSON contract remains versioned separately as v1.

## Baseline UX contracts

- Bare `atlas` is the interactive TUI entry point and must be run from the
  project root.
- If the project already has a complete basic-or-better index
  (`manifest`, `structural`, or `full`), bare `atlas` enters the TUI directly.
- If `.atlas/atlas.db` is missing, empty, or unusable/corrupt, bare `atlas`
  first creates or recovers the database, runs the same default `structural`
  index as `atlas index`, and only then starts the TUI.
- The TUI must visibly show the current project index mode at the edge/status
  area: `empty`, `manifest`, `structural`, `full`, or `partial`.

## Baseline indexing contracts

- Dirty-check for `IndexPipeline` is not hash-only. A discovered file is clean
  only when its on-disk hash matches the DB file hash and the DB has fresh,
  complete file-level `extraction_state` for the requested analysis capability.
- `manifest`, `structural`, and `full` index runs must upgrade hash-clean files
  whose persisted capability is below the requested mode; they must not skip a
  file just because `files.content_hash` is unchanged.
- Missing optional metadata such as `last_index_time` or `last_sync_time` is a
  normal empty-project/fresh-index state and must not produce warnings.
- Atlas uses Schema V3 and intentionally has no runtime migration chain for
  older development schemas. Change the primary DDL and code together, then
  rebuild the project index.

## Baseline focus contracts

- Query-time closure expansion is symbol-scoped. A seed file supplies facts,
  but unrelated peer symbols in that file do not become graph frontiers.
- Import/include dependencies are resolution boundaries first, not automatic
  structural closure members.
- Coverage counts only materialized facts. Successful and failed background
  work both reach a terminal state; failures remain visible as structured gaps.
- Resume refreshes graph snapshots with both foreground and background files.
- Cold type queries return the complete defining scope on the first
  consumable result. Old same-hash structural rows with provably incomplete
  multiline type ranges are rejected and rebuilt on demand.
- TUI native search remains usable before graph readiness. Full snapshot loading and stale
  reload happen in the existing background job system, never in the terminal event loop.

## Reading order

1. [Architecture](./architecture.md) — authoritative architecture: constraints, modules, schema, dataflow, capability profiles, design decisions.
2. [Requirements](./requirements.md) — product scope, default languages, acceptance criteria.
3. [Roadmap](./roadmap.md) — current and future work.
4. [Testing](./testing.md) — test layers, phase requirements, feature matrix.
5. [Performance](./performance.md) — measured baselines and recommendations.
6. [Trace contract](./trace-contract.md) — frozen V1 trace JSON contract and MCP tool schemas.
7. [Domain Rules Language Guide](./domain-rules-language-guide.md) — language registry, rule_kind, pattern, status, and extension guidance.

## Maintenance rules

1. Update `architecture.md` when module boundaries, persistence rules, ID rules, capability profiles, schema version, or design decisions change.
2. Update `requirements.md` when product scope or acceptance criteria change.
3. Update this `docs/README.md` when a baseline user-facing contract changes,
   including entry-point behavior such as bare `atlas` TUI startup.
4. Update `roadmap.md` for current and future work only.
5. Update `testing.md` when release checks or fixture expectations change.
6. Update `trace-contract.md` when trace JSON fields, diagnostics, or capability output change.
7. Update `domain-rules-language-guide.md` when adding a language registry, rule_kind, pattern policy, metadata shape, or learning behavior.
8. Delete obsolete content; do not accumulate archive directories.
9. Treat `db::CURRENT_SCHEMA_VERSION`, `LanguageCapabilityProfile` / `atlas doctor`, and MCP `make_all_tools()` as the executable facts for schema, language capabilities, and tool names.
