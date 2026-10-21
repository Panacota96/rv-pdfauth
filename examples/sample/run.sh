#!/usr/bin/env sh
# Runs the sample review and checks the result. Usage: examples/sample/run.sh [path-to-rv-pdfauth]
set -eu
here=$(cd "$(dirname "$0")" && pwd)
bin=${1:-rv-pdfauth}
out="$here/out"
"$bin" self-test
# Expected exit code 3: the sample set contains high-severity findings by design (see examples/README.md).
rc=0
"$bin" scan --input "$here/evidence" --hash sha256,md5 --lang all --case DEMO-001 --actor "Sample" --out "$out" --progress none || rc=$?
if [ "$rc" -ne 3 ]; then echo "unexpected exit code $rc (expected 3)" >&2; exit 1; fi
run=$(ls "$out/logs" | tail -n 1)
"$bin" verify-log "$out/logs/$run"
echo "Reports: $out/reports/$run/statistics.en.html"
