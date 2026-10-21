#!/usr/bin/env bash
# Toolkit end to end with the REAL intake tool: rv-hashinv registers examples/sample/evidence in a temporary case
# folder, rv-pdfauth analyses every registered item with --case-dir, then both tools run verify-case (exit 0).
# Also checks that a tampered copy is reported as EVIDENCE_HASH_MISMATCH_WITH_CASE and that an edited case log is
# refused by both tools (exit 3).
#
# Not run by CI (it needs an rv-hashinv build; CI covers the same chain in src/cli/tests/toolkit.rs).
#
# Usage: RV_HASHINV_BIN=/path/to/rv-hashinv [RV_PDFAUTH_BIN=/path/to/rv-pdfauth] tests/toolkit_e2e.sh
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
hashinv="${RV_HASHINV_BIN:?set RV_HASHINV_BIN to an rv-hashinv executable (cargo build -p rv-hashinv-cli)}"
pdfauth="${RV_PDFAUTH_BIN:-${CARGO_TARGET_DIR:-$here/target}/debug/rv-pdfauth}"
[ -x "$pdfauth" ] || { echo "rv-pdfauth not built: $pdfauth (cargo build -p rv-pdfauth-cli)" >&2; exit 1; }
export RV_NO_TIMESTAMP=1

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
case_dir="$work/CASE-E2E"
evidence="$work/evidence"
mkdir -p "$evidence"
cp "$here"/examples/sample/evidence/*.pdf "$evidence/"
n_items=$(find "$evidence" -maxdepth 1 -name '*.pdf' | wc -l | tr -d ' ')

fail() { echo "FAIL: $*" >&2; exit 1; }
step() { echo "== $*"; }

# Exit code of a command without stopping the script (set -e).
code() { local c=0; "$@" >"$work/out.txt" 2>&1 || c=$?; echo "$c"; }

step "1. rv-hashinv intake ($n_items items)"
c=$(code "$hashinv" scan -i "$evidence" --case-dir "$case_dir" --case CASE-E2E --jurisdiction fr \
        --hash sha256,md5 --progress none --actor e2e-intake)
cat "$work/out.txt"
[ "$c" = 0 ] || [ "$c" = 3 ] || fail "intake exit $c"
grep -q "registered $n_items (new $n_items)" "$work/out.txt" || fail "intake did not register $n_items items"
[ -f "$case_dir/evidence/registry.json" ] || fail "no registry"

step "2. rv-pdfauth analysis of every registered item"
c=$(code "$pdfauth" analyze --case-dir "$case_dir" --progress none --visual-diff off --actor e2e-analysis)
cat "$work/out.txt"
[ "$c" = 0 ] || [ "$c" = 3 ] || fail "analysis exit $c"
grep -q "(analysis) verified $n_items, mismatched 0, not in registry 0" "$work/out.txt" \
    || fail "not every registered item verified"
ls -d "$case_dir"/runs/rv-pdfauth/*/reports >/dev/null || fail "no rv-pdfauth run folder"

step "3. verify-case by both tools"
c=$(code "$hashinv" verify-case "$case_dir"); cat "$work/out.txt"; [ "$c" = 0 ] || fail "rv-hashinv verify-case exit $c"
c=$(code "$pdfauth" verify-case "$case_dir"); cat "$work/out.txt"; [ "$c" = 0 ] || fail "rv-pdfauth verify-case exit $c"

step "4. tampered copy of a registered file -> EVIDENCE_HASH_MISMATCH_WITH_CASE (exit 3)"
printf '\n%% tampered\n' >> "$evidence/original.pdf"
c=$(code "$pdfauth" analyze --case-dir "$case_dir" -i "$evidence/original.pdf" --progress none --visual-diff off)
cat "$work/out.txt"
[ "$c" = 3 ] || fail "tampered copy: exit $c (expected 3)"
grep -q "mismatched 1" "$work/out.txt" || fail "mismatch not counted"
grep -rq EVIDENCE_HASH_MISMATCH_WITH_CASE "$case_dir"/runs/rv-pdfauth/*/reports/statistics.json \
    || fail "no EVIDENCE_HASH_MISMATCH_WITH_CASE in the reports"
c=$(code "$hashinv" verify-case "$case_dir"); [ "$c" = 0 ] || fail "rv-hashinv verify-case after mismatch run: exit $c"

step "5. edited case log -> verify-case 3 in both tools, analysis refused"
broken="$work/BROKEN"
cp -R "$case_dir" "$broken"
sed -i.bak '1s/CASE-E2E/CASE-XXX/' "$broken/case-log.jsonl" && rm -f "$broken/case-log.jsonl.bak"
c=$(code "$hashinv" verify-case "$broken"); [ "$c" = 3 ] || fail "rv-hashinv verify-case on edited log: exit $c"
c=$(code "$pdfauth" verify-case "$broken"); [ "$c" = 3 ] || fail "rv-pdfauth verify-case on edited log: exit $c"
c=$(code "$pdfauth" analyze --case-dir "$broken" --progress none); [ "$c" = 3 ] || fail "analysis of a broken case: exit $c"

echo "OK  toolkit end to end: rv-hashinv intake -> rv-pdfauth analysis -> verify-case (both tools)"
