# Pulse

Real-time, multilingual news aggregator. Articles stream in from RSS feeds and news
APIs, are embedded and deduplicated as they arrive, and are grouped into evolving
stories that grow, split and merge, all with exactly-once semantics, event-time
watermarks and deterministic replay.

See [docs/PLAN.md](docs/PLAN.md) for the full design and roadmap.

## Layout

```
crates/
  pulse-core/        shared protobuf types, Kafka configs, topic names, article ids
  pulse-cli/         `pulse` developer CLI (doctor, fixture record/stats)
  ingestor/          RSS + GDELT polling → articles.raw
  story-processor/   clustering, checkpoints, replay mode          (phases 3–5, 7)
  story-sink/        stories.events → Postgres                     (phase 6)
  query-api/         Axum REST + SSE                               (phase 6)
embedder/            Python gRPC embedding service (ONNX)          (phase 2)
proto/               protobuf schemas (buf-managed)
config/              sources.toml (feed catalog)
deploy/              docker-compose stack and its config
docs/                design and plan
```

## Prerequisites

- Rust (the version is pinned in `rust-toolchain.toml`; rustup installs it automatically)
- Docker with Compose v2
- Python 3.12+

`protoc` and `buf` don't need to be installed: Rust uses a vendored `protoc`, and
`make proto-lint` runs `buf` in Docker.

## Quick start

```bash
make up        # start the stack, create topics and buckets
make doctor    # verify everything from the host
make check     # fmt, clippy, tests, buf lint, embedder tests
```

`make help` lists all targets. `make nuke` deletes all local data.

## Ingestor

Polls ~130 RSS feeds in 25+ languages (see [config/sources.toml](config/sources.toml))
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

## Local services

| Service | URL | Notes |
|---|---|---|
| Kafka API (Redpanda) | `localhost:19092` | |
| Redpanda Console | http://localhost:8081 | browse topics and messages |
| Redpanda admin/metrics | http://localhost:19644 | |
| Postgres + pgvector | `localhost:5432` | `pulse` / `pulse` |
| S3 (SeaweedFS) | http://localhost:8333 | buckets `pulse-checkpoints`, `pulse-audio` |
| SeaweedFS master | http://localhost:9333 | |
| Prometheus | http://localhost:9090 | scrapes host services on ports 9101–9105 |
| Grafana | http://localhost:3000 | `admin` / `pulse` |

All credentials are local-development values. Real API keys go in `.env`, which is
gitignored.

## Kafka topics

| Topic | Partitions | Retention | Purpose |
|---|---|---|---|
| `articles.raw` | 3 | 30 days | Ingestor output, keyed by article id |
| `articles.embedded` | 1 | forever | Processor input and replay source of truth |
| `articles.late` | 1 | 30 days | Articles beyond allowed lateness |
| `stories.events` | 1 | forever | Story graph changes (transactional) |
