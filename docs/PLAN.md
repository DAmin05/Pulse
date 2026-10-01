# Pulse — Project Plan

Real-time, multilingual news aggregator with streaming story clustering, exactly-once
semantics, event-time correctness, and deterministic replay.

---

## 1. Definition of done

Pulse is finished when these five things can be demonstrated live:

1. **Live feed** — real articles from ~100+ sources in multiple languages cluster into stories within seconds.
2. **Evolving stories** — the story graph visibly grows, splits and merges, with traceable lineage.
3. **Crash safety** — `kill -9` the Story Processor mid-run; final output is byte-identical to an uncrashed run.
4. **Determinism** — replaying any past hour reproduces the same story events (hash-verified).
5. **Measured performance** — Embedder latency/throughput vs. batch size benchmark; live panel for consumer lag, throughput and dedup rate.

Plus one user-facing feature:

6. **Listen in any language** — translate a story briefing into the user's chosen language and play it via ElevenLabs text-to-speech.

---

## 2. Decisions

| Area | Decision |
|---|---|
| Ingestor | Rust |
| Embedder | Python, gRPC, ONNX Runtime |
| Story Processor | Rust, manual epoch checkpointing + Kafka transactions |
| Query API | Rust (Axum), **REST + SSE** |
| Replay | Story Processor binary in `--replay` mode + thin diff service |
| Frontend | TypeScript + React, Sigma.js/Graphology for the graph |
| Broker | Redpanda (Kafka API) for local dev |
| Storage | Postgres + pgvector (no Qdrant) |
| Checkpoints | SeaweedFS (S3 API), local disk fallback |
| Languages | **Multilingual** sources (free RSS available in many languages) |
| Embedding model | `multilingual-e5-small` (384-d, ~100 languages, cross-lingual) |
| TTS | ElevenLabs (on-demand, cached) + browser Web Speech API fallback |
| Translation | DeepL API Free or self-hosted LibreTranslate |
| Deployment | Deferred until the project is complete |

---

## 3. Data sources (all free)

| Source | Use | Limits |
|---|---|---|
| RSS feeds | Primary source | None (poll politely) |
| GDELT DOC 2.0 + 15-min files | Volume, historical backfill, load tests | Free, no key |
| Guardian Open Platform | Rich metadata | Free dev key, non-commercial |
| NYT Times Wire / Top Stories | Structured real-time listing | ~500 req/day |
| Hacker News API | Tech stream | Free, no key |

Multilingual RSS candidates: BBC World Service language services (Mundo, Arabic, Persian,
Hindi, Urdu, Russian, Turkish, Swahili, …), DW (30+ languages), France 24 (en/fr/es/ar),
Al Jazeera (en/ar), RFI, El País, Le Monde, Der Spiegel, ANSA, NHK World.

Skipped: NewsAPI.org free tier (24h delay, localhost only), GNews/Mediastack/NewsData free tiers.

Rules:
- Store title + summary + link only. No full-text scraping. Always link to the original.
- Conditional GET (`ETag` / `If-Modified-Since`), 2–5 min jittered intervals, backoff, honest `User-Agent`.
- Quotas change — verify each when signing up.

---

## 4. Architecture

```
 RSS / GDELT / Guardian / NYT / HN
            │
       [Ingestor] ──► articles.raw (key = article_id)
                            │
                  [Embed loop] ──gRPC──► [Embedder (Python, ONNX)]
                            │
                   articles.embedded   ◄── 1 partition, source of truth for replay
                            │
                   [Story Processor] ── checkpoints ──► SeaweedFS
                            │  (Kafka transactions)
                     stories.events  (+ articles.late, replay.<run>.stories)
                     ┌──────┴───────┐
               [Story Sink]     [Query API] ──SSE──► [Web]
                     │              ▲   │
             Postgres+pgvector ─────┘   └──► Translation + ElevenLabs (on demand, cached in SeaweedFS)
```

- Story Sink reads with `isolation.level=read_committed` and upserts by `event_id` (idempotent).
- Query API never writes story data.
- Translation/TTS is outside the streaming pipeline — it never affects determinism.

### Core schemas (protobuf, `proto/`)

```proto
message Article {
  string id = 1;            // sha256(canonical_url)
  string source_id = 2;
  string url = 3;
  string title = 4;
  string summary = 5;
  int64  published_at = 6;  // EVENT TIME (fallback: fetched_at)
  int64  fetched_at = 7;
  string lang = 8;          // ISO 639-1; feed-declared or detected
}

message EmbeddedArticle {
  Article article = 1;
  repeated float vector = 2;
  string model_version = 3;
}

message StoryEvent {
  string event_id = 1;      // hash(input_offset, seq) — deterministic
  int64  event_time = 2;
  int64  watermark = 3;
  oneof kind {
    StoryCreated created = 10;
    ArticleAdded added = 11;     // includes is_duplicate
    StoryUpdated updated = 12;   // headline / centroid change
    StorySplit split = 13;       // parent → [children]
    StoryMerged merged = 14;     // [sources] → target
    StoryClosed closed = 15;
  }
}
```

---

## 5. Core design

### 5.1 Determinism
- `articles.embedded` has **one partition**. Kafka log order = processing order. (~10k articles/day fits easily.)
- All state uses `BTreeMap` / ordered structures; seeded RNG; no wall-clock reads in processing logic.
- Embeddings are computed once and persisted; replay never re-embeds (batch-size-dependent float drift would break determinism).
- Stretch: multi-partition input with a deterministic `(event_time, article_id)` merge.

### 5.2 Event time & watermarks
- Event time = `published_at`; clamp missing/future timestamps to `fetched_at` (+ small skew), count corrections.
- Watermark = max event time seen − allowed lateness (default 30 min). Derived from the log → deterministic.
- Housekeeping runs on **event-time ticks** (every 10 min): split/merge detection, story closing (48h idle), index/LSH eviction.
- Late within lateness → normal path. Beyond → `articles.late`, attached only if the story is still open. Both counted.

### 5.3 Near-duplicate detection (MinHash LSH)
- Normalize title + summary (Unicode NFKC, lowercase, strip punctuation).
- **Character 5-gram shingles** (works for languages without spaces: zh, ja, th).
- 128 permutations, 16 bands × 8 rows (≈0.7 Jaccard candidate threshold); verify with exact Jaccard ≥ 0.8.
- 72h event-time retention.
- Duplicates are kept and attached to the story with `is_duplicate=true` (coverage signal + dedup metric).
- Exact re-polls (same `article_id`) dropped via a seen-set.
- Note: MinHash is within-language; cross-language grouping is handled by the embeddings.

### 5.4 Online clustering over HNSW
- Index **article vectors** (immutable), not centroids.
- New article → k=10 nearest neighbors → similarity-weighted vote over their stories → join if score ≥ threshold, else create a story.
- e5 cosine scores sit in a compressed high range — thresholds must be tuned on the fixture, not guessed.
- Rebuild the index on housekeeping ticks with active-story articles only (fixed seed, log order).
- Library: `usearch` behind a trait; brute-force cosine oracle for tests. Stretch: own HNSW implementation.
- Cross-lingual: a Spanish and an English article about the same event land in the same story.

### 5.5 Split / merge
- **Split**: for active stories with ≥8 recent articles, build a thresholded kNN graph → connected components (2-means fallback). Two components of ≥3 each, inter-centroid similarity below threshold, both growing → `StorySplit`.
- **Merge**: centroid-pair candidates via a small per-tick centroid index; similarity above threshold for **2 consecutive ticks** → `StoryMerged`.
- Cooldown after split/merge to prevent flapping.
- Stories keep `parent_ids` → lineage DAG.
- Headline = medoid article's title; per-language representative headlines where available.

### 5.6 Exactly-once & checkpointing
Epoch loop (every ~5s or 500 messages):

```
begin_transaction()
  consume → process → produce StoryEvents (inside txn)
at epoch end:
  1. serialize state → checkpoint/{input_offset}.bin   (fsync + upload)
  2. send_offsets_to_transaction(input_offset)
  3. commit_transaction()
```

Recovery: committed offset `O` → load `checkpoint/O.bin` → delete checkpoints > `O` → seek to `O`.
- Crash before commit → txn aborted, outputs invisible, resume from previous checkpoint.
- Crash after commit → checkpoint for `O` exists (written before commit).

State: stories, centroids, article index, LSH buckets, watermark, merge candidates, RNG state.
Serialized with `bincode` or `rkyv`. Checkpoints indexed by offset and watermark, retained 7 days.

### 5.7 Embedder
- `multilingual-e5-small` ONNX on CPU; inputs prefixed `passage: ` (articles) / `query: ` (search).
- Dynamic batching: asyncio queue, flush on `max_batch` or `max_wait_ms`.
- Benchmark grid: `max_batch ∈ {1,4,8,16,32,64}` × `max_wait_ms ∈ {0,2,5,10,20}` × fp32/int8 → p50/p99 latency vs. throughput chart.
- Prometheus metrics: queue depth, batch size histogram, latency, throughput.

### 5.8 Query API (Axum, REST + SSE)

| Endpoint | Purpose |
|---|---|
| `GET /stories?active=true&lang=&limit=` | Current stories |
| `GET /stories/:id` | Story + articles + lineage |
| `GET /stories/search?q=` | Semantic search (cross-lingual, pgvector) |
| `GET /graph?as_of=<ts>&window=1h` | Graph reconstructed from the event log |
| `GET /stream` | SSE stream of live `StoryEvent`s |
| `POST /replays`, `GET /replays/:id` | Trigger a replay, get the determinism diff |
| `GET /metrics/summary` | Aggregated Prometheus data for the dashboard |
| `POST /stories/:id/briefing` | `{lang, voice}` → translated text + audio URL |

Time travel:
1. **Scrub** — `as_of` reconstructs from `story_events` (instant).
2. **Re-run** — "Replay this hour" triggers a real pipeline replay and shows the diff.

### 5.9 Replay
- `story-processor --replay --from T1 --to T2 --output replay.<run_id>.stories`
- Restore nearest checkpoint ≤ T1, reprocess to T2, write to an isolated topic.
- Diff service compares against `stories.events` for the range: counts, per-event hashes, first divergence.
- Target output: "N events, 0 diffs".

### 5.10 Listen in any language (Translation + ElevenLabs TTS)
- Unit = **story briefing** (headline + 2–3 representative summaries), not full articles.
- Flow: pick source text (prefer an article already in the target language) → translate if needed → ElevenLabs multilingual TTS → store MP3 in SeaweedFS (S3).
- Cache key: `(story_id, story_version, lang, voice)` — repeat plays cost nothing.
- Translation: DeepL API Free (monthly character quota) or self-hosted LibreTranslate (unlimited, lower quality). Behind a trait so either works.
- ElevenLabs free tier has a small monthly character quota (verify current limits and attribution terms) → enforce a per-day budget; on exhaustion fall back to the browser's Web Speech API.
- API keys live in `.env` (gitignored): `ELEVENLABS_API_KEY`, `DEEPL_API_KEY`.

---

## 6. Phases

| # | Phase | Scope | Exit criterion |
|---|---|---|---|
| 0 | Foundations | Cargo workspace, `pulse-core`, `buf` + protos, docker-compose (Redpanda, Postgres+pgvector, SeaweedFS, Prometheus, Grafana), CI | `make up` works, CI green |
| 1 | Ingestor | Source trait, RSS adapter, `sources.toml` (~100+ multilingual feeds), language detection, GDELT adapter, metrics | 24h unattended run; **record 24h of `articles.raw` as the golden fixture** |
| 2 | Embedder | gRPC service, dynamic batching, ONNX model, embed loop, benchmark | Benchmark chart; fixture embedded |
| 3 | Processor v1 | Dedup + clustering + story events (no fault tolerance yet) | Sensible stories on fixture (incl. cross-lingual); HNSW recall ≥ 0.95 vs. oracle |
| 4 | Correctness | Watermarks, late handling, epoch transactions, checkpoints, recovery | Determinism test + 50-kill chaos test pass in CI |
| 5 | Split / merge | Ticks, split/merge with hysteresis, lineage | Hand-verified events on fixture; thresholds tuned |
| 6 | Sink + API | Story Sink, Postgres schema, Axum endpoints, SSE | `curl /stream` shows live events; `as_of` works |
| 7 | Replay | Replay mode, checkpoint index, diff service | "Replay last hour → 0 diffs" via API |
| 8 | Frontend | Story graph, time-travel slider, metrics panel, story drawer | Demo items 1–5 work in the browser |
| 9 | Listen | Translation + ElevenLabs briefing endpoint, cache, budget, Web Speech fallback, player UI | Play a story in ≥3 languages; cached replays cost no credits |
| 10 | Polish | README, architecture diagram, benchmark and exactly-once write-ups, demo video | A stranger can clone and run the demo |

Phases 1 and 2 can run in parallel. Phase 4 comes before split/merge on purpose.
Deployment is deliberately out of scope until phase 10 is done.

---

## 7. Testing

| Test | Proves |
|---|---|
| Golden fixture (24h recorded `articles.raw`) | Reproducible, offline development |
| Determinism: fixture twice → `sha256` of outputs equal | Determinism |
| Chaos: random `kill -9` during fixture run → output equals clean run | Exactly-once |
| Late data: perturbed event times within/beyond lateness | Watermark logic |
| `proptest`: MinHash vs. exact Jaccard; state serialize/deserialize round-trip | Primitives |
| HNSW vs. brute-force oracle | Recall numbers |
| Labeled set (~300 articles, multiple languages) → purity / ARI (optional) | Story quality |
| Fixture replay at 10× / 100× | Throughput and lag ceilings |

---

## 8. Risks

| Risk | Mitigation |
|---|---|
| Threshold tuning (esp. e5's compressed similarity range) | Fast fixture-replay tuning loop from phase 3 |
| HNSW lib nondeterministic / not serializable | Trait abstraction; brute-force fallback; own implementation |
| rdkafka transaction edge cases | Short epochs, `transaction.timeout.ms` > checkpoint upload time, chaos test |
| Split flapping | Min component sizes, hysteresis, cooldowns |
| Bad feed timestamps | Clamp + count |
| Language detection errors on short text | Prefer feed-declared language; detect on title + summary |
| TTS / translation quota exhaustion | Cache, daily budget, Web Speech fallback |
| Scope creep | LLM summaries, multi-partition processing, own HNSW = stretch goals after phase 10 |
