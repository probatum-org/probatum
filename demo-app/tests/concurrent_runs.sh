#!/usr/bin/env bash
# Launch N probatum runs at once and assert each reserved its own evidence
# directory. Run numbering used to be max+1, which two racing processes could
# both read before either created anything.
set -u
N=${1:-20}
BIN=./target/debug/probatum
before=$(ls .probatum/runs 2>/dev/null | wc -l)

for _ in $(seq 1 "$N"); do
  printf '[[check]]\nrun = "true"\n' | "$BIN" run - >/dev/null 2>&1 &
done
wait

after=$(ls .probatum/runs 2>/dev/null | wc -l)
created=$((after - before))
reports=$(ls .probatum/runs/*/run.json 2>/dev/null | wc -l)

echo "created $created run dirs for $N runs, $reports report(s) total"
[ "$created" -eq "$N" ] || { echo "FAIL: $created dirs for $N runs — collision"; exit 1; }
[ "$reports" -ge "$N" ] || { echo "FAIL: only $reports run.json"; exit 1; }
