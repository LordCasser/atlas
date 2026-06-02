# Atlas documentation

This directory keeps the release-facing documentation that should remain current for V1 users.

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
3. Update `roadmap.md` for current and future work only.
4. Update `testing.md` when release checks or fixture expectations change.
5. Update `trace-contract.md` when trace JSON fields, diagnostics, or capability output change.
6. Update `domain-rules-language-guide.md` when adding a language registry, rule_kind, pattern policy, metadata shape, or learning behavior.
7. Delete obsolete content; do not accumulate archive directories.
