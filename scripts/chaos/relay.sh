#!/usr/bin/env bash
# Chaos test for the embed relay's exactly-once guarantee.
#
# Reprocesses all of articles.raw into a scratch topic with a fresh consumer
# group. Each round starts the relay, waits until it has committed progress,
# then SIGKILLs it at a random moment (usually mid-transaction). A final run
# drains the input. The committed output must contain exactly one record per
# input article.
#
# Requires: stack up (`make up`), Embedder running, articles.raw non-empty.
# Usage: scripts/chaos/relay.sh [kills=5]
set -euo pipefail
cd "$(dirname "$0")/../.."

KILLS=${1:-5}
RUN=chaos-$(date +%s)
TOPIC=test.$RUN.embedded
RELAY=./target/release/embed-relay
PULSE=./target/release/pulse
LOG=$(mktemp "${TMPDIR:-/tmp}/relay-chaos.XXXXXX")

rpk() { docker compose -f deploy/docker-compose.yml exec -T redpanda rpk "$@"; }

# Sum of the group's committed offsets on articles.raw ("-" = none yet).
committed() {
  rpk group describe "$RUN" 2>/dev/null |
    awk '$1 == "articles.raw" && $3 ~ /^[0-9]+$/ { s += $3 } END { print s + 0 }'
}

start_relay() {
  PULSE_RELAY_GROUP=$RUN PULSE_RELAY_OUTPUT_TOPIC=$TOPIC PULSE_RELAY_METRICS_PORT=0 \
    RUST_LOG=info "$RELAY" >>"$LOG" 2>&1 &
  echo $!
}

progressed_past() { (($(committed) > $1)); }
drained() { (($(committed) == expected)); }

wait_for() { # wait_for <seconds> <command...>: poll until the command succeeds
  local deadline=$((SECONDS + $1)); shift
  until "$@"; do
    ((SECONDS < deadline)) || return 1
    sleep 0.5
  done
}

cargo build -q --release -p embed-relay -p pulse-cli
rpk topic create "$TOPIC" -p 1 >/dev/null
expected=$(rpk topic describe articles.raw -p | awk 'NR > 1 { s += $NF } END { print s }')
echo "run=$RUN input=$expected articles output=$TOPIC kills=$KILLS log=$LOG"

for i in $(seq 1 "$KILLS"); do
  before=$(committed)
  pid=$(start_relay)
  if ! wait_for 90 progressed_past "$before"; then
    echo "  round $i: no progress within 90s"; kill -9 "$pid"; exit 1
  fi
  # Progress made; now die at a random point inside a later batch.
  sleep "$(awk -v s="$RANDOM" 'BEGIN { srand(s); printf "%.2f", 0.2 + rand() * 2.5 }')"
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  echo "  kill $i: committed $(committed)/$expected"
  if [[ "$(committed)" == "$expected" ]]; then echo "  input fully committed; stopping kills"; break; fi
done

pid=$(start_relay)
wait_for 300 drained || echo "  final run did not drain"
kill -INT "$pid"; wait "$pid" 2>/dev/null || true
echo "  final run: committed $(committed)/$expected"

result=$("$PULSE" topic check "$TOPIC" || true)
echo "$result" | sed 's/^/  /'
records=$(echo "$result" | awk '/^records/ {print $2}')
dupes=$(echo "$result" | awk '/^duplicate keys/ {print $3}')

if [[ "$records" == "$expected" && "$dupes" == "0" ]]; then
  rpk topic delete "$TOPIC" >/dev/null
  rpk group delete "$RUN" >/dev/null 2>&1 || true
  echo "PASS: $records/$expected articles, 0 duplicates across $KILLS kill -9 rounds"
else
  echo "FAIL: $records/$expected articles, $dupes duplicate keys (kept $TOPIC; log: $LOG)"
  exit 1
fi
