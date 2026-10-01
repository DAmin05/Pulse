#!/usr/bin/env bash
# Creates Pulse topics. Idempotent: existing topics are left alone.
# Keep in sync with crates/pulse-core/src/topics.rs (`pulse doctor` checks it).
set -euo pipefail

THIRTY_DAYS_MS=2592000000

create() {
  local name=$1 partitions=$2 retention=$3
  if rpk topic describe "$name" >/dev/null 2>&1; then
    echo "exists   $name"
  else
    rpk topic create "$name" -p "$partitions" -r 1 -c "retention.ms=$retention" >/dev/null
    echo "created  $name (partitions=$partitions retention.ms=$retention)"
  fi
}

create articles.raw       3 "$THIRTY_DAYS_MS"
# Source of truth for the Story Processor and replay: single partition, kept forever.
create articles.embedded  1 -1
create articles.late      1 "$THIRTY_DAYS_MS"
create stories.events     1 -1
