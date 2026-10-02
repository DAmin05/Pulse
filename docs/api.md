# Read model and API

The Query API (Rust, Axum) serves everything the UI shows, on port 9105. All
read endpoints accept `?at=<offset>` or `?as_of=<RFC 3339 time>` for time travel.

The **sink** applies `articles.embedded` and `stories.events` to Postgres. Each batch's
rows **and the consumed offsets** commit in one transaction, and the sink resumes
from offsets stored in Postgres, so it's exactly-once without Kafka commits. Every
statement is also idempotent (tested by applying a whole history twice).

**History is versioned by log position.** Each article's story membership is stored
as a validity range `[from_offset, to_offset)` in `articles.embedded`, so splits and
merges close rows instead of overwriting them, and any past state is a query: pass
`?at=<offset>` or `?as_of=<RFC 3339 time>` to any read endpoint. "Now" is just the
latest position, so live views and time travel share one code path.

| Endpoint | Returns |
|---|---|
| `GET /api/stories?sort=sources\|size\|recent&lang=&min_sources=&limit=` | Story cards: headline, languages, counts, top sources, 24h activity sparkline, lineage ids |
| `GET /api/stories/{id}` | Articles (with duplicate/late flags), parents, children, merged from/into, event history |
| `GET /api/graph?limit=&min_sources=&min_similarity=` | Nodes + similarity edges (centroid cosine) + split edges + recent splits/merges |
| `GET /api/search?q=` | Cross-lingual semantic search (e5 query → pgvector), grouped by story; `strong` flags real matches |
| `GET /api/timeline?buckets=` | Articles / new stories / splits / merges / closes per time bucket (every bucket, empty ones included), with the offset to jump to |
| `GET /api/stats` | Totals, Kafka end offsets, sink position and lag per topic |
| `GET /api/pipeline` | Pipeline panel: rates, latencies and watermark lag from Prometheus (latest + 30 min series), consumer lag per stage (cached 5 s) |
| `GET /api/sources` | Feed catalog with article counts and last fetch |
| `POST /api/replays` · `GET /api/replays[/{id}]` | Re-drive a window of the input log and diff it against the live output (see Replay) |
| `GET /api/listen` · `POST /api/stories/{id}/briefing` · `GET /api/audio/{key}` | Story briefings translated and read aloud (see Listen) |
| `GET /api/stream` | **SSE** of committed story events (`event: story`, `id: <offset>:<seq>`); resume with `Last-Event-ID` or `?after=` |
| `GET /metrics` | Prometheus (request latency per route, live clients) |

The stream is fed by a Postgres `NOTIFY` that the sink sends **inside its
transaction**, so a streamed event is always already queryable. There's no race
between "event arrived" and "story not in the DB yet". A reconnecting client
replays exactly what it missed (verified: 966 events, in order, no duplicates),
then continues live.

Verified live: with `make pipeline`, 846 fresh articles flowed from RSS to the SSE
stream in 4 minutes (622 new stories, 220 joins, 2 live merges), with zero warnings
from any service and sink lag 0.
