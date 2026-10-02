#!/usr/bin/env bash
# Chaos test for the Story Processor: exactly-once output and determinism
# under crashes.
#
# 1. Publishes a fixture into a scratch input topic.
# 2. Clean run: default settings, never interrupted.
# 3. Chaos run: tiny epochs and frequent snapshots, SIGKILLed at random
#    moments (usually mid-transaction), restarted each time (restore the latest
#    snapshot, silently replay the gap, continue).
# 4. PASS iff both runs' story events and late articles are byte-identical, in
#    order. Crashes, restores, epoch sizes and snapshot timing must not change
#    a single byte of output.
#
# Requires: stack up (`make up`). No Embedder needed: the input is pre-embedded.
# Usage: scripts/chaos/processor.sh [kills=10] [fixture=data/fixtures/synth.pulseem]
set -euo pipefail
cd "$(dirname "$0")/../.."

KILLS=${1:-10}
FIXTURE=${2:-data/fixtures/synth.pulseem}
RUN=pchaos-$(date +%s)
IN=test.$RUN.input
PROC=./target/release/story-processor
PULSE=./target/release/pulse
TMP=$(mktemp -d "${TMPDIR:-/tmp}/processor-chaos.XXXXXX")

rpk() { docker compose -f deploy/docker-compose.yml exec -T redpanda rpk "$@"; }

cargo build -q --release -p story-processor -p pulse-cli
[[ -f "$FIXTURE" ]] || "$PULSE" fixture synth --out "$FIXTURE" --count 5000
rpk topic create "$IN" -p 1 >/dev/null
for v in clean chaos; do
  rpk topic create "test.$RUN.$v.events" "test.$RUN.$v.late" -p 1 >/dev/null
done
"$PULSE" fixture publish --file "$FIXTURE" --topic "$IN" >/dev/null
END=$(rpk topic describe "$IN" -p | awk 'NR > 1 { print $NF }')
echo "run=$RUN input=$END records from $FIXTURE kills=$KILLS dir=$TMP"

# start <variant> [extra env...]: launches a processor in the background, prints its pid.
start() {
  local v=$1; shift
  env PULSE_PROCESSOR_GROUP="$RUN-$v" PULSE_PROCESSOR_INPUT_TOPIC="$IN" \
    PULSE_PROCESSOR_OUTPUT_TOPIC="test.$RUN.$v.events" \
    PULSE_PROCESSOR_LATE_TOPIC="test.$RUN.$v.late" \
    PULSE_PROCESSOR_SNAPSHOT_DIR="$TMP/$v" PULSE_PROCESSOR_METRICS_PORT=0 \
    RUST_LOG=info "$@" "$PROC" run >>"$TMP/$v.log" 2>&1 &
  echo $!
}

committed() {
  rpk group describe "$RUN-$1" 2>/dev/null |
    awk -v t="$IN" '$1 == t && $3 ~ /^[0-9]+$/ { print $3; found = 1 } END { if (!found) print 0 }'
}
progressed_past() { (($(committed "$1") > $2)); }
drained() { (($(committed "$1") >= END)); }
wait_for() { # wait_for <seconds> <command...>
  local deadline=$((SECONDS + $1)); shift
  until "$@"; do
    ((SECONDS < deadline)) || return 1
    sleep 0.2
  done
}
stop() { kill -INT "$1" 2>/dev/null || true; wait "$1" 2>/dev/null || true; }

# --- clean run ---------------------------------------------------------------
pid=$(start clean)
wait_for 600 drained clean || { echo "clean run did not drain"; exit 1; }
stop "$pid"
echo "  clean run: committed $(committed clean)/$END"

# --- chaos run ---------------------------------------------------------------
CHAOS_ENV=(PULSE_PROCESSOR_MAX_BATCH=50 PULSE_PROCESSOR_LINGER_MS=20
           PULSE_PROCESSOR_SNAPSHOT_EVERY_MESSAGES=300)
kills=0
for i in $(seq 1 "$KILLS"); do
  before=$(committed chaos)
  pid=$(start chaos "${CHAOS_ENV[@]}")
  wait_for 120 progressed_past chaos "$before" || { echo "  round $i: no progress"; kill -9 "$pid"; exit 1; }
  sleep "$(awk -v s="$RANDOM" 'BEGIN { srand(s); printf "%.2f", rand() * 0.6 }')"
  kill -9 "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  kills=$((kills + 1))
  now=$(committed chaos)
  printf "  kill %2d: committed %s/%s\n" "$i" "$now" "$END"
  if ((now >= END)); then echo "  input fully committed; stopping kills"; break; fi
done
pid=$(start chaos "${CHAOS_ENV[@]}")
wait_for 600 drained chaos || echo "  final chaos run did not drain"
stop "$pid"
restores=$(grep -c "state recovered" "$TMP/chaos.log" || true)
replayed=$({ grep -o "replayed=[0-9]*" "$TMP/chaos.log" || true; } | awk -F= '{ s += $2 } END { print s + 0 }')
echo "  chaos run: committed $(committed chaos)/$END after $kills kills, $restores starts, $replayed inputs replayed"

# --- compare -----------------------------------------------------------------
fail=0
for kind in events late; do
  clean=$("$PULSE" topic hash "test.$RUN.clean.$kind")
  chaos=$("$PULSE" topic hash "test.$RUN.chaos.$kind")
  if [[ "$clean" == "$chaos" ]]; then
    echo "  $kind: identical ($clean)"
  else
    echo "  $kind: DIFFERENT  clean=$clean  chaos=$chaos"
    fail=1
  fi
done
"$PULSE" topic check "test.$RUN.chaos.events" >/dev/null || { echo "  duplicate event ids in chaos output"; fail=1; }

# Replay the second half of each run against its committed output: the clean
# run warms up from the log's start, the chaos run from one of its snapshots.
for v in clean chaos; do
  if env PULSE_PROCESSOR_GROUP="$RUN-$v" PULSE_PROCESSOR_INPUT_TOPIC="$IN" \
      PULSE_PROCESSOR_OUTPUT_TOPIC="test.$RUN.$v.events" PULSE_PROCESSOR_LATE_TOPIC="test.$RUN.$v.late" \
      PULSE_PROCESSOR_SNAPSHOT_DIR="$TMP/$v" RUST_LOG=warn \
      "$PROC" replay --from $((END / 2)) >"$TMP/replay-$v.json" 2>"$TMP/replay-$v.txt"; then
    echo "  replay ($v, second half): $(grep '^events' "$TMP/replay-$v.txt")"
  else
    echo "  replay ($v) DIFFERENT:"; sed 's/^/    /' "$TMP/replay-$v.txt"; fail=1
  fi
done

if ((fail)); then
  echo "FAIL (kept topics test.$RUN.*; logs in $TMP)"
  exit 1
fi
rpk topic delete -r "test\\.$RUN\\..*" >/dev/null
rpk group delete "$RUN-clean" "$RUN-chaos" >/dev/null 2>&1 || true
rm -rf "$TMP"
echo "PASS: output byte-identical to a clean run after $kills kill -9s"
