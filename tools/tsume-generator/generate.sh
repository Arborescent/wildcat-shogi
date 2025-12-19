#!/bin/bash

OUTPUT="${1:-results.sfen}"
TOTAL="${2:-1000}"
WORKERS=16
PER_WORKER=$(( (TOTAL + WORKERS - 1) / WORKERS ))
TMPDIR=$(mktemp -d)

echo "Generating $TOTAL tsume with $WORKERS workers ($PER_WORKER each)..."

for i in $(seq 1 $WORKERS); do
    ./target/release/tsume-generator "$TMPDIR/part_$i.sfen" "$PER_WORKER" &
done

wait

cat "$TMPDIR"/part_*.sfen | sort -u > "$OUTPUT"
rm -rf "$TMPDIR"

echo "Done: $(wc -l < "$OUTPUT") unique tsume -> $OUTPUT"
