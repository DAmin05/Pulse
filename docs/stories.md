# How stories form

The Story Processor turns a stream of embedded articles into stories that grow,
split and merge. This is the clustering design and how it was tuned; for the
crash-safety side see [exactly-once.md](exactly-once.md), and for speed and
recall see [benchmarks.md](benchmarks.md).

## Clustering

```bash
pulse fixture record --topic articles.embedded --since 72h --out data/fixtures/x.pulseem
make cluster-eval EMBEDDED=data/fixtures/x.pulseem   # stories, quality stats, weakest joins
make cluster-sweep EMBEDDED=...                     # threshold grid
make ann-recall EMBEDDED=...                        # HNSW vs brute force
embed-relay embed-fixture in.pulsefx out.pulseem    # embed a raw fixture offline
```

For each embedded article, in log order:

1. **Exact duplicate** (same id): ignored.
2. **Near duplicate** (MinHash LSH on character 5-grams, 128 hashes in 16×8 bands,
   estimated Jaccard ≥ 0.8): attached to the original's story as a syndicated copy.
   It stays out of the vector index and the centroid.
3. **Vote:** the 10 nearest articles in a deterministic **HNSW** index (our own
   implementation: seeded levels, ties broken by key) vote for their stories. The
   winner is joined if the article also fits the story's centroid. Otherwise it
   starts a new story.

What made clustering work:

- **Per-language mean centering.** Raw e5 vectors are anisotropic: unrelated articles
  in the same language score about 0.78 cosine, and each language has its own offset,
  so same-language topic clusters crowd out cross-lingual matches of real events.
  Subtracting frozen per-language means (`config/centering/<model>.json`, fit with
  `story-processor calibrate`) centers unrelated pairs at 0. That **tripled
  cross-lingual stories** at equal coverage.
- **Drift guards.** Joining an established story (3+ articles) needs 2 of its members
  among the neighbors, and centroid fit within 0.10 of the story's own cohesion.
  Without them, chains of locally similar headlines grow into "anything about 2027
  finances". With them, the largest GDELT cluster shrinks from 160 to 100 articles
  while multi-source stories increase.

| fixture | articles | stories with ≥2 sources | cross-lingual | HNSW recall@10 (ef 64) | throughput |
|---|---:|---:|---:|---:|---:|
| RSS, 132 feeds, 72h | 3,989 | 302 | 118 | 0.996 | ~900/s |
| GDELT translingual, 4h | 49,220 | 5,477 | 1,236 | 0.978 | ~700/s |

Output is deterministic: the same fixture gives the same event hash on every run
and across debug/release builds.

## Event time, exactly-once and recovery

```bash
make process                          # live: articles.embedded → stories.events (+ articles.late)
make chaos-processor KILLS=10         # kill -9 repeatedly; output must match a clean run byte for byte
pulse fixture synth --out x.pulseem   # deterministic synthetic stream (late, dupes, drifting topics)
pulse fixture publish --file x.pulseem --topic <topic>
pulse topic hash <topic>              # order-sensitive fingerprint of committed records
```

- **Watermark** = max event time − 24h allowed lateness, derived only from the log.
  A **late** article (behind the watermark) can join an open story, flagged `late`,
  but never creates one. Instead it goes to `articles.late`. On the cold-start RSS
  fixture, 6h lateness dropped 52% of inputs as late, 24h drops 17% (only the stale
  backlog), and 48h drops 5%.
- **Housekeeping on event-time ticks** (every 10 min of watermark, never wall clock):
  stories idle for 48h close (`StoryClosed`) and their articles leave memory. The
  HNSW index tracks evicted entries as tombstones and is rebuilt once they pass 20%,
  so state stays bounded without per-eviction rebuilds.
- **Exactly once:** each epoch (≤500 inputs) commits its story events, its late
  articles and the next input offset in one Kafka transaction.
- **Log-structured checkpoints:** state (zstd + bincode, ~11 MB for 4k articles) is
  snapshotted every 5,000 inputs or 5 minutes, *after* a commit. On restart, the
  processor loads the newest snapshot at or before the committed offset and
  **silently replays** the gap from Kafka; determinism makes the rebuilt state
  identical. So the log is the write-ahead log, and snapshots can be rare while
  commits stay frequent. A snapshot records a fingerprint of the config and
  centering, and resuming it under different settings is refused.
- **Proof:** a property test snapshots at random points, restores, and checks that
  events *and final state bytes* equal an uninterrupted run. `scripts/chaos/processor.sh`
  runs the real thing: a clean run vs a run SIGKILLed up to 50 times, with tiny
  epochs and frequent snapshots. Both output topics must have identical hashes. CI
  runs 50 kills on 20k synthetic articles.

## Lineage: splits and merges

```bash
story-processor lineage --fixture data/fixtures/x.pulseem   # every split/merge with headlines + similarity probes
make reset-processor                                        # clear processor state + outputs after config/schema changes
```

Every 100 inputs (a position in the log, so deterministic; and it's when stories
change), stories that changed since the last check are re-evaluated:

- **Split:** a deterministic 2-means over a story's indexed members. If both halves
  have ≥3 members and their centroids are less than 0.50 similar, the story splits:
  `StorySplit` lists which articles go to which child, each child gets a
  `StoryCreated` with `parent_story_ids`, and the parent gets `StoryClosed(SPLIT)`.
- **Merge:** established stories whose centroids are ≥0.72 similar *and* share an
  **anchor word**, or ≥0.82 without one. A word anchors a pair when at least half
  of the live articles using it are in those two stories: "Flydubai" or "Bardella"
  qualify, "budget", "2027" or "Sendung" don't. That separates real same-event
  fragments from template look-alikes at similar embedding scores.
- **Anti-flapping:** a candidate must qualify on 2 consecutive checks; the merge
  threshold sits well above the split threshold; and stories involved sit out 6
  checks afterwards.

Tuning results: RSS gets 4 merges, all correct (FlyDubai fragments, Bardella/Mediapart,
the Russian Christa Pike story into the English one) and 0 splits; its most
separable large story is still one event (halves 0.74 similar). On headline-only
GDELT, most of the 59 merges are correct cross-lingual ones (Vietnamese → Spanish
helicopter crash, Malvinas, Plavšić, Pakistan–Afghanistan), but template clusters
formed at join time (budgets by country, TV listings, horoscopes, ballot numbers)
merge with each other. Fixing those needs entity- or content-type features, not
thresholds. Split threshold 0.50 gives no false splits; at 0.55 GDELT split one
event by language.

Snapshot/restore stays exact with lineage active: the property test includes
split-heavy and merge-heavy configurations, and the chaos test passes on real data
while merges are happening.

Known limitations: same-template events in different countries ("government
presents 2027 budget") share a story on headline-only data, and lineage can't
separate them; evergreen genres (horoscopes) cluster together. Both are rare in the
curated RSS feeds and common in GDELT.
