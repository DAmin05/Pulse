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
  pulse-cli/         `pulse` developer CLI (`pulse doctor`)
  ingestor/          RSS/API polling → articles.raw                (phase 1)
  story-processor/   clustering, checkpoints, replay mode          (phases 3–5, 7)
  story-sink/        stories.events → Postgres                     (phase 6)
  query-api/         Axum REST + SSE                               (phase 6)
embedder/            Python gRPC embedding service (ONNX)          (phase 2)
proto/               protobuf schemas (buf-managed)
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
