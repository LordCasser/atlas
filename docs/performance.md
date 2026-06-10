# Atlas Performance Baseline

## Test Environment

- **Machine**: Apple Silicon (aarch64), macOS
- **Atlas build**: `cargo build --release -p atlas-cli`
- **Date**: 2026-05-23

## Baseline 1: TypeScript Project (project-graph)

### Project Profile
- **Files**: 165 discovered, 146 indexed
- **Languages**: TypeScript (183), JavaScript (1)
- **Symbols extracted**: 1,704
- **References extracted**: 11,819
- **Resolution rate**: 78.7% (9,299 resolved / 2,520 unresolved)

### Phase Timings

| Phase | Time | % of Total | Notes |
|-------|------|-----------|-------|
| Discovery | 12ms | 0.1% | 165 items |
| Hash check | 3ms | 0.0% | 0 reused, 165 dirty |
| Parse/extract | 592ms | 6.4% | avg 3.6ms/file |
| DB write | 2,190ms | 23.6% | 146 items inserted |
| Resolution | 5,957ms | **64.3%** | 11,819 refs → 9,299 resolved |
| Graph build | 496ms | 5.4% | 9,708 edges |
| **Total** | **9,264ms** | 100% | |

### Resolution Breakdown
| Strategy | Count | % |
|----------|-------|---|
| fuzzy_match | 6,714 | 72.2% |
| import_resolved | 1,123 | 12.1% |
| name_only | 846 | 9.1% |
| exact_match | 616 | 6.6% |

### Memory
- Max RSS: 176 MB
- Peak footprint: 171 MB

### Issues
- **19 files failed**: `FOREIGN KEY constraint failed` in DB batch insert
- **Resolution bottleneck**: 64% of wall time spent in resolution, primarily fuzzy matching

---

## Baseline 2: Multi-Language Project (Atlas Itself)

### Project Profile
- **Files**: 156 indexed
- **Languages**: 11 (Rust 129, Go 4, C# 4, Kotlin 4, PHP 4, Ruby 4, TypeScript 3, ArkTS 1, C 1, JavaScript 1, Python 1)
- **Symbols extracted**: 5,065
- **References extracted**: 27,786
- **Resolution rate**: 74.8% (20,779 resolved / 7,007 unresolved)

### Phase Timings

| Phase | Time | % of Total | Notes |
|-------|------|-----------|-------|
| Discovery | 13ms | 0.0% | 156 items |
| Hash check | 3ms | 0.0% | |
| Parse/extract | 253ms | 0.9% | avg 1.6ms/file |
| DB write | 2,207ms | 7.9% | 156 items |
| Resolution | 22,300ms | **79.4%** | 27,786 refs → 20,779 resolved |
| Graph build | 3,367ms | 12.0% | 19,618 edges |
| **Total** | **28,100ms** | 100% | |

### Resolution Breakdown
| Strategy | Count | % |
|----------|-------|---|
| fuzzy_match | 15,184 | 73.1% |
| name_only | 3,997 | 19.2% |
| import_resolved | 946 | 4.6% |
| exact_match | 652 | 3.1% |

### Per-Language Parse/Extract Speed
| Language | Files | Time | avg/file |
|----------|-------|------|----------|
| Rust | 129 | 1,922ms | 14.9ms |
| Ruby | 4 | 236ms | 59ms |
| Kotlin | 4 | 233ms | 58ms |
| C# | 4 | 213ms | 53ms |
| TypeScript | 3 | 118ms | 39ms |
| PHP | 4 | 75ms | 19ms |
| JavaScript | 1 | 34ms | 34ms |
| ArkTS | 1 | 21ms | 21ms |
| Python | 1 | 17ms | 17ms |
| Go | 4 | 9ms | 2.3ms |
| C | 1 | 7ms | 7ms |

- **All 7 new languages extracted with 0 errors**

### Memory
- Max RSS: ~200 MB (estimated from peak)

---

## Key Findings

### 1. Resolution is the dominant bottleneck
Across both projects, resolution consumes 64-79% of total wall time. Within resolution, **fuzzy matching** accounts for 72-73% of resolved references.

- **Recommendation**: Investigate fuzzy match algorithm (O(n²) name similarity checks likely driving cost). Consider:
  - Index-based candidate pre-filtering
  - Per-language resolution scope limits
  - Parallel execution of resolution sub-phases

### 2. DB write overhead
Consistently ~2.2s for 146-156 files, regardless of language count. This suggests fixed per-transaction overhead dominates.

- **Recommendation**: Profile batch insert transaction vs individual inserts. Consider larger batches or lower isolation level.

### 3. New language extraction is fast and reliable
All 6 new DataflowBasic languages (Go, C#, Rust, PHP, Ruby, Kotlin) extracted correctly with 0 errors across the Atlas codebase itself. Parse/extract times are reasonable:
- Fastest: Go (2.3ms/file), C (7ms/file)
- Slowest: Ruby (59ms/file), Kotlin (58ms/file), C# (53ms/file)

### 4. DB integrity issue
The TypeScript project had 19 files fail with `FOREIGN KEY constraint failed`. This indicates a data integrity bug in the batch insert path that needs investigation.

### 5. 168-file project completes in <10 seconds
A mid-size TypeScript project indexes in under 10 seconds. This is acceptable for interactive use but the resolution bottleneck must be addressed before scaling to 500+ file projects (which would take 30-60+ seconds).

---

## Recommended Next Steps

1. **[P0] Fix DB integrity**: Investigate FOREIGN KEY failures in batch insert
2. **[P1] Optimize resolution**: Focus on fuzzy match performance (72% of resolution cost)
3. **[P2] Profile DB write**: Reduce 2.2s fixed overhead
4. **[P3] Scale test**: Run on 500+ file project to validate linearity

---

## Performance Optimization Methodology

> Lessons learned from the 2026-06-10 optimization cycle that reduced a 1,931-file
> TypeScript project index from 53.3s to 31.3s (41% improvement). These are
> process-level principles, not code-specific recipes.

### 0. The Golden Rule: Measure First, Optimize Never in the Dark

The most expensive mistake in performance work is optimizing the wrong thing.
Before writing a single line of optimization code:

1. **Make the hot path observable.** Add lightweight instrumentation (counters,
   timers, spans) to the critical path. Without this, you're guessing.
2. **Get a release-mode baseline.** Debug builds and tracing-enabled runs have
   overhead that can mask or distort real bottlenecks — see §5.
3. **Profile on a real workload.** A 100-file synthetic test tells you nothing
   about a 2,000-file monorepo. Use the largest project you have.

In our cycle, the initial performance plan identified 10 candidate optimizations
based on static code analysis. Only 4 survived A/B verification. One was
targeting a code path that didn't exist (SummaryBuilder O(N²) — the code had
already been fixed). Two introduced regressions larger than their benefit. The
remaining 3 were neutral. The real breakthrough (import resolution caches,
responsible for >70% of the 41% improvement) was **not in the original plan at all**
— it was discovered through strategy timing counters added during profiling.

### 1. Cumulative CPU Time ≠ Wall Clock Time

In parallel systems, a bottleneck may be invisible in wall clock but obvious in
cumulative CPU. Example from our profiling:

| Strategy | Calls | Wall Impact | Cumulative CPU | % of CPU |
|----------|-------|-------------|----------------|----------|
| S6 (global+fuzzy) | 176,860 | ~12.7s | 48.0s | 47% |
| S5 (import) | 26,716 | ~3.3s | 53.4s | 53% |

Wall clock alone would suggest S6 is the dominant bottleneck (12.7s vs 3.3s).
But cumulative CPU reveals S5 costs more total work — it's just better spread
across threads. After caching, S5 cumulative dropped from 253s to 53s: the 200s
saving was invisible in wall clock because it was parallelized, but it freed
CPU capacity that other work could use.

**Rule**: In any parallel system, always measure both wall clock AND cumulative
CPU (per-thread aggregation). The wall clock bottleneck and the CPU bottleneck
are often different things.

**Rule**: In any parallel system, always measure both wall clock AND cumulative
CPU (per-thread aggregation). The wall clock bottleneck and the CPU bottleneck
are often different things.

### 2. A/B Testing Discipline

Every optimization claim must survive a clean A/B comparison:

```
Baseline (zero changes) ──→ 53.29s
Proposed change         ──→ 51.84s  ← claim: -2.7%
```

Essential controls:
- **Identical input.** Same project, same source files, same `.git` state.
- **Clean state.** Delete `.atlas/` database before every run. Stale caches
  and incremental paths contaminate measurements.
- **Release build.** `--release` only. Debug mode has bounds checks and no
  inlining; tracing subscribers add per-span overhead (see §5).
- **Minimal logging.** `RUST_LOG=warn` or equivalent. Every `info!()` call
  that hits the subscriber costs time.
- **Run multiple times.** Single-run variance on a live system can be ±5%.
  Look for consistent direction, not absolute numbers.

When a change shows a regression, **verify by reverting** before moving on.
A single misattributed regression can lead to hours of dead-end investigation.
In our cycle, we reverted and re-verified every significant change at least once.

### 3. Progressive Isolation: Revert One Variable at a Time

When multiple optimizations are applied together and the result is a regression,
the fastest way to find the culprit is **progressive revert**:

```
Full optimized:    81.07s  (+13.7% vs baseline)  ← something is wrong
Disable T0.1:      75.23s  (+5.5%)               ← T0.1 costs 5.8s
Disable T0.2:      77.83s  (+9.2%)               ← T0.2 saves 2.6s
Disable T1.1:      52.45s  (-26.4%)              ← T1.1 costs 22.8s!
```

Without this isolation, you'd either discard all optimizations (throwing away
the +2.6s benefit of T0.2) or ship a regression (accepting the +22.8s cost of
T1.1). You'd have no way to know which parts are working.

**Rule**: When benchmarking a group of changes, make each independently
revertible. A boolean flag, a feature gate, or a git stash — whatever lets you
toggle one variable without affecting others.

### 4. Thread-Local Caches Beat Mutex-Guarded Shared Caches

In a parallel system with 8 worker threads and a read-dominated cache:

```
Mutex<HashMap> shared cache:  +5.8s regression (contention at 249% CPU)
thread_local RefCell cache:   zero measurable overhead
```

The intuition that "shared cache has better hit rate" is correct in theory but
wrong in practice for this workload: each rayon thread processes a different
file's references, and import symbols cluster by module (same file, same thread).
Thread-local caches achieve nearly the same hit rate as a shared cache for
import resolution, without any cross-thread synchronization.

**Rule**: Before reaching for a shared cache with locking, measure whether
thread-local caches have sufficient locality for your access pattern. If they
do, the elimination of contention is almost always worth any reduction in hit
rate.

### 5. Tracing Spans Are Not Free

`tracing` spans have runtime cost even when the log level filters them out:

| Configuration | Hot Path Overhead |
|---------------|-------------------|
| No spans | Baseline |
| `debug_span!` on 19K-call path, RUST_LOG=warn | +16% wall clock |
| `info_span!` on 3-call path, RUST_LOG=warn | negligible |

The overhead comes from span creation, field evaluation, and enter/exit hooks
— all of which execute before the subscriber decides whether to record the span.
For high-frequency paths (thousands of calls per second), even `debug_span!`
with no active subscriber can measurably slow things down.

**Rule**: Use `info_span!` for phase-level instrumentation (a few calls per
run). For hot-path measurement, use `AtomicU64` counters or `Instant::now()`
timers aggregated at the end — orders of magnitude cheaper than spans.

### 6. database Batching: Test Your Assumptions

Three separate batching approaches were attempted. All three regressed:

| Approach | Expected | Actual | Root Cause |
|----------|----------|--------|------------|
| Dynamic SQL batch INSERT | Faster | +22.8s | SQLite prepared statement cache beats dynamic SQL construction |
| Subquery batch DELETE | Faster | +330s | Missing index caused full table scan on subquery |
| Batch symbol lookup (IN clause) | Faster | +2.2s | Same prepared-statement advantage as INSERT |

SQLite's internal prepared statement cache makes per-row `stmt.execute(params![])`
extremely efficient — the statement is compiled once, cached, and reused. Dynamic
SQL (building `INSERT INTO ... VALUES (?,?,?), (?,?,?)...`) defeats this by
creating a new, unique statement for each batch size. The larger the batch, the
more expensive the compilation.

**Rule**: SQLite batching is not always faster. If the per-row path uses a
fixed prepared statement that SQLite can cache, it may outperform "smarter"
batching. Always measure. This is counter-intuitive for engineers coming from
PostgreSQL or MySQL where batching is almost always a win.

### 7. Cut Negative Experiments Quickly

Of 11 optimization attempts, 5 were verified regressions and 2 were neutral.
Average time from implementation to verified rejection: ~20 minutes per attempt.

The discipline:
1. Implement the change (minimal, focused)
2. Build release
3. Run A/B on the target project
4. If regression or neutral → **revert immediately, move on**
5. If improvement → dig deeper, refine, re-verify

Don't fall in love with an idea. The levenshtein banding optimization (T0.2)
looked promising and delivered +2.6s. The batch INSERT optimization (T1.1)
looked equally promising and delivered +22.8s regression. Both passed initial
code review. Only measurement revealed which was which.

**Rule**: An optimization idea is a hypothesis, not a plan. Treat it like one:
design a falsifiable test, run it, accept the result.

### 8. Strategy Distribution Data Is More Valuable Than Timing Data

The single most important piece of profiling data from this cycle was NOT the
timing breakdown — it was the **strategy hit distribution**:

```
S1 (Builtin):      7,616 ( 2.4%)  ← dead simple, no optimization needed
S2 (Scope-local): 24,462 ( 7.8%)  ← moderate, adequate
S3 (Container):        0 ( 0.0%)  ← DEAD CODE for TypeScript
S4 (Same-file):   31,113 ( 9.9%)  ← moderate, O(1) sufficient
S5 (Import):      26,716 ( 8.5%)  ← 8.5% of calls, 53% of CPU → THE BOTTLENECK
S6 (Global/fuzzy):176,860 (56.3%) ← 56% of calls, 47% of CPU → already efficient
MISS:             47,218 (15.0%)  ← unresolvable, wasted work
```

Without this distribution, you'd naturally focus on S6 (largest call count).
But S6 is 56% of calls and 47% of CPU — each call is cheap (271μs). S5 is 8.5%
of calls and 53% of CPU — each call is expensive (2ms). The distribution data
redirected attention from call volume to per-call cost.

**Rule**: Before timing anything, count everything. A 7-counter `AtomicU64`
array costs nothing and tells you where to point the timers.

### 9. Summary Checklist

Before starting any performance optimization:

- [ ] Add lightweight counters (AtomicU64) on the hot path to measure
      distribution — what % of calls go through each code path?
- [ ] Add timing on each code path (Instant::now() → AtomicU64 aggregation)
      to measure per-call cost — which paths are expensive vs frequent?
- [ ] Establish a release-mode baseline on the largest real project available.
      Document the exact command, project state, and environment.
- [ ] Clean state before every run (delete databases, caches, incrementals).
- [ ] For each proposed optimization: make it independently revertible before
      measuring. If it regresses, revert immediately and document why.
- [ ] Trust measurements over intuition. If static analysis says "this should
      be faster" but the benchmark disagrees, the benchmark is right.
