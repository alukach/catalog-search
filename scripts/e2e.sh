#!/usr/bin/env bash
# Checks the running API against jq run over the raw catalog: two independent routes to the
# same answers. Needs `npm run dev` up, the index loaded, and catalog.jsonl present.
#
#   scripts/e2e.sh [base-url] [catalog.jsonl]
set -u
B=${1:-http://127.0.0.1:8787}
C=${2:-catalog.jsonl}
O=$(dirname "$0")/oracle.jq
fail=0

eq() { # name got want
  if [ "$2" = "$3" ]; then echo "ok    $1 ($2)"; else echo "FAIL  $1: api=$2 jq=$3"; fail=1; fi
}
# The oracle file ends in the filter for what the indexer keeps; later pipes narrow it.
indexed() { jq -c "$(cat "$O") | $1" "$C" | wc -l | tr -d ' '; }
matched() { curl -sG "$B/collections" "$@" | jq .numberMatched; }

eq "everything" "$(matched)" "$(indexed '.id')"
eq "keyword (A_OVERLAPS)" \
  "$(matched --data-urlencode "filter=A_OVERLAPS(keywords, ('geoparquet'))")" \
  "$(indexed 'select((.keywords//[])|index("geoparquet")) | .id')"
eq "license AND keyword" \
  "$(matched --data-urlencode "filter=license = 'CC-BY-4.0' AND A_CONTAINS(keywords, ('geoparquet'))")" \
  "$(indexed 'select(.license=="CC-BY-4.0" and ((.keywords//[])|index("geoparquet"))) | .id')"
eq "size range" \
  "$(matched --data-urlencode "filter=total_bytes BETWEEN 1000000 AND 1000000000")" \
  "$(indexed 'select(.total_bytes >= 1000000 and .total_bytes <= 1000000000) | .id')"

for spec in "Europe|-10,35,30,60" "World|-180,-90,180,90" "Australia|100,-40,160,-10" "Antarctica|-180,-90,180,-60" "Paris|2.3,48.8,2.4,48.9"; do
  IFS='|' read -r name bb <<<"$spec"; IFS=, read -r w s e n <<<"$bb"
  eq "bbox $name" "$(matched --data-urlencode "bbox=$bb")" "$(indexed "select(hits($w;$s;$e;$n)) | .id")"
  eq "S_INTERSECTS $name" \
    "$(matched --data-urlencode "filter=S_INTERSECTS(geometry, BBOX($w,$s,$e,$n))")" \
    "$(indexed "select(hits($w;$s;$e;$n)) | .id")"
done
# A box that crosses the antimeridian is two boxes.
eq "bbox across the antimeridian" "$(matched --data-urlencode "bbox=170,-20,-170,20")" \
  "$(indexed 'select(hits(170;-20;180;20) or hits(-180;-20;-170;20)) | .id')"

code() { curl -s -o /dev/null -w '%{http_code}' "$@"; }
eq "GET a collection" "$(code "$B/collections/addresscloud/epc")" 200
eq "GET it with %2F" "$(code "$B/collections/addresscloud%2Fepc")" 200
eq "unknown collection" "$(code "$B/collections/nope/nothing")" 404
eq "unknown queryable" "$(code -G "$B/collections" --data-urlencode "filter=bogus = 1")" 400
eq "problem content type" \
  "$(curl -s -o /dev/null -w '%{content_type}' -G "$B/collections" --data-urlencode "limit=0")" application/problem+json
eq "write is refused" "$(code -X POST "$B/collections")" 405

exit $fail
