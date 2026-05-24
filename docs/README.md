# Atlas documentation

This directory keeps the release-facing and contributor-facing documentation that should remain current for V1 users. Completed milestones, superseded plans, and phase logs are archived under [`archive/`](archive/). Active planning remains in `05-roadmap.md`.

## Recommended reading order

1. [Requirements](./01-requirements.md)
2. [Architecture constraints](./02-architecture-constraints.md)
3. [Current architecture](./03-current-architecture.md)
4. [Roadmap](./05-roadmap.md)
5. [Testing spec](./07-testing-spec.md)
6. [Performance baseline](./08-performance-baseline.md)
7. [Trace contract](./trace-contract.md)

## Archived development notes

The following files are retained for project history, but they are not part of the current user-facing documentation set:

- [Architecture and requirements change log](./archive/04-changes.md)
- [Historical roadmap](./archive/05-roadmap.md)
- [Phase log](./archive/06-phase-log.md)
- [Inter-procedural dataflow design notes](./archive/09-interprocedural-dataflow-design.md)
- [Multi-language dataflow implementation plan](./archive/dataflow-implementation-plan.md)

## Maintenance rules

1. Update `01-requirements.md` when product scope or acceptance criteria change.
2. Update `02-architecture-constraints.md` when module boundaries, persistence rules, ID rules, or graph/resolution constraints change.
3. Update `03-current-architecture.md` when implemented code structure, schema, CLI/MCP behavior, or analysis capability changes.
4. Update `05-roadmap.md` for current and future work only; move completed or superseded roadmap sections to `archive/`.
5. Update `07-testing-spec.md` when release checks, fixture expectations, or feature validation requirements change.
6. Update `08-performance-baseline.md` only when performance methodology or measured release baselines change.
7. Update `trace-contract.md` when CLI/MCP trace JSON fields, diagnostics, or capability output change.
8. Move obsolete planning notes to `archive/` instead of linking them from the README as active guidance.
