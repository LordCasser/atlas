# Atlas documentation

This directory keeps the release-facing documentation that should remain current for V1 users.

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

## Reading order

1. [Architecture](./architecture.md) — authoritative architecture: constraints, modules, schema, dataflow, capability profiles, design decisions.
2. [Requirements](./requirements.md) — product scope, MVP languages, acceptance criteria.
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
