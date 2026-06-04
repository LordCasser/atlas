#!/usr/bin/env bash
# xtask/examples-smoke.sh — Examples regression smoke test.
#
# Cleans old databases, re-indexes all checked-in examples, and reports a
# matrix with minimum gate thresholds. Designed for manual or nightly CI use.
#
# Usage:
#   ./xtask/examples-smoke.sh              # full clean + index + report
#   ./xtask/examples-smoke.sh --report     # report only (DBs must exist)
#   ATLAS=./target/release/atlas ./xtask/examples-smoke.sh
#
# Minimum gates per full-analysis language example:
#   - files > 0
#   - data_nodes > 0
#   - dataflow_edges > 0
#   - bindings > 0 (warn only for non-lexical languages)
#   - FK violations = 0

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ATLAS="${ATLAS:-$ROOT/target/debug/atlas}"
MODE="${1:-full}"

EXAMPLES=(
    arkts_example
    c_example
    c_sharp_example
    cangjie_example
    go_example
    python_example
    java_example
    rust_example
    typescript_example
)

# ── Build if needed ──
if [ ! -x "$ATLAS" ]; then
    echo "Building atlas-cli (all-languages)..."
    cargo build -p atlas-cli --features all-languages --manifest-path "$ROOT/Cargo.toml"
fi

# ── Clean + Index (skip if --report) ──
if [ "$MODE" != "--report" ]; then
    echo "=== Cleaning old databases ==="
    for d in "${EXAMPLES[@]}"; do
        rm -rf "$ROOT/examples/$d/.atlas"
    done

    echo ""
    echo "=== Indexing examples ==="
    for d in "${EXAMPLES[@]}"; do
        echo "  $d ..."
        if [ ! -d "$ROOT/examples/$d" ]; then
            echo "  missing example directory: $ROOT/examples/$d"
            continue
        fi
        timeout 120 "$ATLAS" index --analysis full --project "$ROOT/examples/$d"
    done
fi

# ── Report ──
echo ""
echo "=== Results ==="
printf "%-18s %6s %8s %10s %9s %11s %14s %3s\n" \
    "example" "files" "bindings" "sym_edges" "data_nodes" "dataflow_edges" "references" "FK"
printf "%-18s %6s %8s %10s %9s %11s %14s %3s\n" \
    "------------------" "------" "--------" "----------" "---------" "-----------" "--------------" "---"

PASS=0; FAIL=0

for d in "${EXAMPLES[@]}"; do
    db="$ROOT/examples/$d/.atlas/atlas.db"
    if [ ! -f "$db" ]; then
        printf "%-18s %s\n" "$d" "NOT INDEXED"
        FAIL=$((FAIL + 1))
        continue
    fi

    files=$(sqlite3 "$db" "SELECT COUNT(*) FROM files;" 2>/dev/null || echo 0)
    binds=$(sqlite3 "$db" "SELECT COUNT(*) FROM bindings;" 2>/dev/null || echo 0)
    sedges=$(sqlite3 "$db" "SELECT COUNT(*) FROM symbol_edges;" 2>/dev/null || echo 0)
    dns=$(sqlite3 "$db" "SELECT COUNT(*) FROM data_nodes;" 2>/dev/null || echo 0)
    des=$(sqlite3 "$db" "SELECT COUNT(*) FROM dataflow_edges;" 2>/dev/null || echo 0)
    refs=$(sqlite3 "$db" "SELECT COUNT(*) FROM \"references\";" 2>/dev/null || echo 0)
    fk=$(sqlite3 "$db" "SELECT COUNT(*) FROM pragma_foreign_key_check;" 2>/dev/null || echo 0)

    printf "%-18s %6s %8s %10s %9s %11s %14s %3s\n" \
        "$d" "$files" "$binds" "$sedges" "$dns" "$des" "$refs" "$fk"

    lf=0
    [ "$files" -gt 0 ] || { echo "  FAIL: files=0"; lf=1; }
    [ "$binds" -gt 0 ] || echo "  WARN: bindings=0 (lexical may be unsupported)"
    [ "$dns" -gt 0 ]   || { echo "  FAIL: data_nodes=0"; lf=1; }
    [ "$des" -gt 0 ]   || { echo "  FAIL: dataflow_edges=0"; lf=1; }
    [ "$fk" -eq 0 ]    || { echo "  FAIL: FK violations=$fk"; lf=1; }
    [ "$lf" -eq 0 ] && PASS=$((PASS + 1)) || FAIL=$((FAIL + 1))
done

echo ""
echo "=== $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ] || exit 1
