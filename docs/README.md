# Atlas documentation

This directory keeps the release-facing and contributor-facing documentation that should remain current for V1 users.

## Reading order

1. [Requirements](./01-requirements.md) — product scope, MVP languages, acceptance criteria
2. [Architecture constraints](./02-architecture-constraints.md) — module boundaries, ID rules, fact model, invariants
3. [Current architecture](./03-current-architecture.md) — implemented state, dataflow, schema, authoritative capability table
4. [Roadmap](./04-roadmap.md) — current V1 work and future product lines
5. [Testing spec](./05-testing-spec.md) — test layers, phase requirements, feature matrix
6. [Performance baseline](./06-performance-baseline.md) — measured baselines and recommendations
7. [Trace contract](./07-trace-contract.md) — frozen V1 trace JSON contract and MCP tool schemas

## Maintenance rules

1. Update `01-requirements.md` when product scope or acceptance criteria change.
2. Update `02-architecture-constraints.md` when module boundaries, persistence rules, or ID rules change.
3. Update `03-current-architecture.md` when code structure, schema, or language capability profiles change.
4. Update `04-roadmap.md` for current and future work only.
5. Update `05-testing-spec.md` when release checks or fixture expectations change.
6. Update `06-performance-baseline.md` when performance methodology or measured baselines change.
7. Update `07-trace-contract.md` when trace JSON fields, diagnostics, or capability output change.
8. Delete obsolete content; do not accumulate archive directories.
