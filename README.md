# Pulse

Real-time, multilingual news aggregation. Articles stream in from 130+ RSS feeds,
are embedded and deduplicated as they arrive, and group into stories that grow,
split and merge across languages. Everything is exactly once, event-time correct
and deterministic: kill any service mid-write and the output doesn't change by a
byte, and any past hour can be replayed and checked.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="docs/img/pulse-dark.png">
  <img alt="Pulse: ranked stories, the story graph with one story focused, and its details with the Listen player" src="docs/img/pulse-light.png">
</picture>

## What it does

- **Live feed:** articles from ~130 sources in 20+ languages cluster into stories within seconds.
- **Evolving stories:** the graph grows, splits and merges live, with traceable lineage.
- **Crash safety:** `kill -9` any stage mid-run; the output stays byte-identical to an uncrashed run.
- **Determinism:** "Re-run this hour" replays the input log and proves the same events come out, hash for hash.
- **Measured performance:** batching benchmark for the embedder; live panel for lag, throughput and latency.
- **Listen in any language:** a 30-second briefing, translated (DeepL) and read aloud (ElevenLabs), with a live transcript.

## Results

| claim | evidence |
|---|---|
| Exactly once under crashes | 50 SIGKILLs during 20,000 inputs: 22,294 story events and 460 late articles byte-identical to a clean run (in CI) |
| Deterministic replay | Whole live history replayed: 4,912 / 4,912 events identical; a 0.01 threshold change is caught |
| Relay exactly once | 5 kills mid-transaction: 7,490 / 7,490 articles, 0 duplicates |
| Batching that helps on CPU | Length bucketing: 213 texts/s (int8) where naive batching drops to 149 |
| Cross-lingual clustering | Per-language centering tripled cross-lingual stories; HNSW recall@10 0.996 |

Write-ups: [exactly once](docs/exactly-once.md) · [benchmarks](docs/benchmarks.md) · [how stories form](docs/stories.md) · [architecture](docs/architecture.md) · [API](docs/api.md) · [listen](docs/listen.md) · [development](docs/development.md)

## Architecture

![Pulse architecture](docs/architecture.svg)

Rust for the ingestor, relay, processor, sink and API; Python for the embedding
model (ONNX, gRPC); TypeScript + React for the web app. Kafka (Redpanda) topics
connect the stages; Postgres + pgvector holds the read model, versioned by log
offset so time travel is a query. Details: [docs/architecture.md](docs/architecture.md).

## Quick start

Prerequisites: Docker with Compose v2, Rust (rustup; the toolchain is pinned),
Python 3.12+, Node 22 with pnpm (`corepack enable`). About 1 GB of disk for the
model and images.

```bash
make up            # Kafka, Postgres, SeaweedFS, Prometheus, Grafana; creates .env
make model         # once: downloads multilingual-e5-small (~450 MB) and builds int8
make web-install   # once
make pipeline      # terminal 1: every service (Ctrl-C stops all)
make web           # terminal 2: http://localhost:5173
```

The first poll fetches up to 72 hours of backlog from every feed, so within a few
minutes there are thousands of articles and hundreds of multi-source stories.
Optional: put `DEEPL_API_KEY` and `ELEVENLABS_API_KEY` in `.env` for translated,
natural-voice briefings; without them the browser's own voice reads them.

`make check` runs every check CI runs. `make help` lists all targets; `make nuke`
deletes all local data.

## Local services

| Service | URL | Notes |
|---|---|---|
| Kafka API (Redpanda) | `localhost:19092` | |
| Redpanda Console | http://localhost:8081 | browse topics and messages |
| Redpanda admin/metrics | http://localhost:19644 | |
| Postgres + pgvector | `localhost:5432` | `pulse` / `pulse` |
| S3 (SeaweedFS) | http://localhost:8333 | buckets `pulse-checkpoints`, `pulse-audio` |
| SeaweedFS master | http://localhost:9333 | |
| Query API | http://localhost:9105/api | REST + SSE; `/metrics` too |
| Frontend (dev) | http://localhost:5173 | `make web` |
| Prometheus | http://localhost:9090 | scrapes host services on ports 9101–9106 |
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
