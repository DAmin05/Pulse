# Development guide

Commands for working on each part of Pulse. `make help` lists every target.
Design write-ups: [architecture](architecture.md), [exactly once](exactly-once.md),
[stories](stories.md), [benchmarks](benchmarks.md), [API](api.md), [listen](listen.md).

## Checks

```bash
make check        # cargo fmt + clippy (-D warnings) + tests, buf lint, embedder ruff + pytest, web lint + tests
make test-db      # read-model integration tests against the local Postgres
make web-check    # eslint + tsc + vitest only
```

CI runs the same checks plus the full stack: `make up`, `pulse doctor`, the
database tests, and the processor chaos test (50 kills on 20,000 synthetic
articles).

## Ingestor

Polls 134 RSS feeds in 26 languages (see [config/sources.toml](../config/sources.toml))
and publishes `pulse.v1.Article` messages to `articles.raw`, keyed by article id.

```bash
make sources-check   # poll every source once, print a health table, publish nothing
make ingest          # run continuously (metrics on :9101/metrics)
make fixture-record  # snapshot the last 24h of articles.raw into data/fixtures/
```

- **Politeness:** conditional GET (ETag / Last-Modified), first polls staggered across
  the interval, ±10% jitter, at most 2 concurrent requests per host, exponential
  backoff on errors, `Retry-After` honored.
- **No duplicate publishing:** URLs are canonicalized before hashing into ids. A seen
  set skips items already published and is rebuilt from `articles.raw` on startup, so
  restarts don't republish.
- **Event time:** the publisher's timestamp. Missing or future timestamps fall back to
  fetch time and are flagged `event_time_corrected`. Items older than `max_age` (72h)
  are skipped.
- **Language:** configured per source, then the feed's declared language, then
  detection (`detect_lang = true` for mixed-language feeds). Normalized to ISO 639-1.
- **GDELT:** off by default for live use (high volume, mostly local news, headlines
  only). Use it for load-test fixtures:

  ```bash
  cargo run -p ingestor --release -- backfill-gdelt --stream translingual \
    --from 2026-10-01T00:00:00Z --to 2026-10-01T06:00:00Z --out data/fixtures/gdelt.pulsefx
  ```

  Fixture output is deterministic for a given range: ordered by `(fetched_at, id)`,
  with `fetched_at` set to the slot's end time.

Add a feed by appending a `[[source]]` block, then run
`cargo run -p ingestor -- check --source <id>`.

## Embedding

```bash
make model        # download multilingual-e5-small (pinned revision, sha256-checked) + build int8
make embedder     # gRPC on :50061, metrics on :9102
make relay        # articles.raw → articles.embedded, exactly once
make chaos-relay  # kill -9 the relay 5 times mid-run; verify 1 output per input
make bench FIXTURE=data/fixtures/<file>.pulsefx
```

- **Model:** `multilingual-e5-small` (384 dimensions, ~100 languages, shared vector space
  across languages). Inputs get the `passage: ` / `query: ` prefixes e5 was trained
  with. Mean pooling, then L2 normalization. `model_version` records the revision,
  precision and token limit, since all three change the vectors.
- **Dynamic batching:** a batch closes at `max_batch` texts or `max_wait_ms` after its
  first request, whichever comes first. Inference runs on one worker thread, so the
  next batch fills while the current one runs.
- **Length bucketing:** each batch is sorted by token length and split into
  sub-batches of at most `PULSE_EMBEDDER_TOKEN_BUDGET` padded tokens (default 1024).
- **Exactly once:** the relay embeds each batch of `articles.raw`, then writes the
  vectors and the consumed offsets in **one Kafka transaction**. A crash anywhere
  either commits both or neither. On error it aborts and exits, and a restart resumes
  from the last commit. `scripts/chaos/relay.sh` proves this with repeated `kill -9`.
- **Vectors are computed once.** Downstream consumers and replay read them from
  `articles.embedded` and never re-embed, because results can shift slightly with
  batch composition.

`pulse topic check <topic>` counts committed records and duplicate ids in any topic.

Benchmarks and their write-up: [benchmarks.md](benchmarks.md).

## Story Processor

```bash
pulse fixture record --topic articles.embedded --since 72h --out data/fixtures/x.pulseem
make cluster-eval EMBEDDED=data/fixtures/x.pulseem   # stories, quality stats, weakest joins
make cluster-sweep EMBEDDED=...                     # threshold grid
make ann-recall EMBEDDED=...                        # HNSW vs brute force
embed-relay embed-fixture in.pulsefx out.pulseem    # embed a raw fixture offline
story-processor lineage --fixture x.pulseem         # every split/merge with headlines + similarity probes
make process                                        # live: articles.embedded → stories.events (+ articles.late)
make reset-processor                                # clear processor state + outputs after config changes
```

How clustering works: [stories.md](stories.md).

## Crash testing

```bash
make chaos-relay                      # kill -9 the relay 5 times mid-run; 1 output per input
make chaos-processor KILLS=10         # kill -9 the processor; output must match a clean run byte for byte
pulse fixture synth --out x.pulseem   # deterministic synthetic stream (late, dupes, drifting topics)
pulse fixture publish --file x.pulseem --topic <topic>
pulse topic hash <topic>              # order-sensitive fingerprint of committed records
pulse topic check <topic>             # committed records and duplicate ids
```

## Replay

```bash
make replay                       # whole history vs what the live processor committed
make replay FROM=4500             # a window (input offsets), warmed up from a live snapshot
story-processor replay --from-time 2026-10-01T22:34:00Z --output-topic replay.x.stories
story-processor replay --perturb-neighbor-similarity 0.49   # shows the diff catching a change
curl -X POST localhost:9105/api/replays -d '{"from_time":"…","to_time":"…"}' -H 'content-type: application/json'
```

A replay restores the newest live snapshot at or before `from` (or starts from the
log's beginning, which is always valid), silently processes up to `from`, then
re-drives `[from, to)`. It compares the result **byte for byte, in order** with the
events the live processor committed to `stories.events` for the same inputs, and
with the articles it routed to `articles.late`. `to` is clamped to the live
processor's committed offset. The report gives counts, an order-sensitive hash of
each side, and the first divergence if any. Replays only read live topics; they can
write to an isolated `replay.<id>.stories` topic (24h retention) with the same
format as live output.

On the live history (the cold-start backlog plus live RSS, with the processor
restarted from snapshots several times along the way):

| replay | warm-up | events | late | result |
|---|---|---:|---:|---|
| whole history `[0, 5037)` | from scratch | 4,912 / 4,912 | 704 / 704 | **identical** (6.1 s) |
| `[4500, 5037)` | snapshot @4020 + 404 inputs | 513 / 513 | 1 / 1 | **identical** (2.3 s) |
| whole history, `neighbor_similarity` 0.50 → 0.49 | from scratch | 928 / 4,912 match | 702 / 704 | **different**: first at input 413, same join with a different vote score |

The perturbed run is the control: a diff that can't fail proves nothing. Through
the API, replays run one at a time in the background, their reports are stored in
Postgres, and runs interrupted by an API restart are marked failed. CI runs replays
of both the clean and the crash-tested processor output inside the chaos test.

## Frontend

```bash
make web-install
make web                          # http://localhost:5173, proxies /api to :9105
PULSE_API=http://localhost:9115 make web   # point it at another API
make web-check                    # eslint + tsc + vitest
```

React 19 + TypeScript, Vite, TanStack Query for reads and one `EventSource` for the
live stream. Everything on screen is a view of a log position: "live" follows the
latest one, and any other position is time travel through the same endpoints.

- **Story graph.** A d3-force simulation drawn on canvas. Area tracks article
  count, color tracks recency, and important stories sit near the center. Live
  events animate in place: new stories are born (or burst out of a split parent),
  articles pulse their story, and merges fly the absorbed story into its target
  along a dashed lineage line. Hover for a summary; click to focus a story and its
  neighbours.
- **Feed and drawer.** Stories ranked by distinct sources, with a live activity
  ticker. The drawer shows coverage over 24h, languages, lineage links, every
  article (syndicated copies and late arrivals flagged) and the story's event history.
- **Search (⌘K).** Semantic and cross-lingual: a Spanish query finds English coverage.
- **Timeline.** Input per interval over the whole history, with merges and splits
  marked. Drag or use the arrow keys to time travel, press play to watch history
  unfold, or "Re-run this hour" to replay that window through the processor and see
  the byte-for-byte verdict with both hashes. Stretches when the pipeline was off
  are shown as such, not as quiet news.
- **Pipeline panel.** Each stage with its consumer lag, plus throughput, latency
  and watermark lag sparklines from Prometheus.

Light and dark themes (palette checked for color-blind safety and contrast),
keyboard access throughout, `prefers-reduced-motion` respected, and layouts down to
phone width.

## Grafana dashboard

The "Pulse pipeline" dashboard is provisioned from
`deploy/grafana/provisioning/dashboards/pulse.json`, generated by
`deploy/grafana/build_dashboard.py`. Edit the script, run it, then
`docker compose -f deploy/docker-compose.yml restart grafana`.
