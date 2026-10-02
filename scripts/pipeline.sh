#!/usr/bin/env bash
# Runs the whole Pulse pipeline in the foreground with prefixed logs:
#   embedder → ingestor → embed-relay → story-processor → story-sink → query-api
# Ctrl-C stops every service gracefully (each finishes its current batch).
#
# Requires: `make up` (Kafka, Postgres, …) and `make model` (embedding model).
# Usage: scripts/pipeline.sh [--no-ingest]   (--no-ingest: process existing data only)
set -euo pipefail
cd "$(dirname "$0")/.."

INGEST=1
[[ "${1:-}" == "--no-ingest" ]] && INGEST=0

[[ -f .env ]] && { set -a; . ./.env; set +a; }
export RUST_LOG=${RUST_LOG:-info}

cargo build -q --release -p ingestor -p embed-relay -p story-processor -p story-sink -p query-api
make -s py-proto >/dev/null

pids=()
colors=(36 33 35 32 34 31)

# run <name> <command...>: start in the background with a colored log prefix.
run() {
  local name=$1; shift
  local color=${colors[${#pids[@]} % ${#colors[@]}]}
  ( "$@" 2>&1 | while IFS= read -r line; do
      printf '\033[%sm%-16s\033[0m %s\n' "$color" "$name" "$line"
    done ) &
  pids+=($!)
}

stop() {
  trap - INT TERM
  echo
  echo "stopping pipeline…"
  # Reverse order: producers stop first, then their consumers drain.
  for ((i = ${#pids[@]} - 1; i >= 0; i--)); do
    pkill -INT -P "${pids[$i]}" 2>/dev/null || true
  done
  pkill -INT -f "target/release/(ingestor|embed-relay|story-processor|story-sink|query-api)" 2>/dev/null || true
  pkill -INT -f "pulse_embedder" 2>/dev/null || true
  wait 2>/dev/null || true
  echo "stopped"
  exit 0
}
trap stop INT TERM

run embedder bash -c 'cd embedder && PYTHONPATH=src exec .venv/bin/python -m pulse_embedder'
echo "waiting for the embedder…"
for _ in $(seq 1 120); do
  curl -sf "localhost:${PULSE_EMBEDDER_METRICS_PORT:-9102}/metrics" >/dev/null && break
  sleep 0.5
done

((INGEST)) && run ingestor ./target/release/ingestor run
run embed-relay ./target/release/embed-relay
run processor ./target/release/story-processor run
run sink ./target/release/story-sink
run api ./target/release/query-api

echo "pipeline up: API http://localhost:${PULSE_API_PORT:-9105}/api/stories · stream /api/stream · Ctrl-C to stop"
wait
