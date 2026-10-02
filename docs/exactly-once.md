# Exactly once, end to end

Pulse turns a stream of news articles into a stream of story events (created,
article added, split, merged, closed) that users see live and can replay. Two
promises hold across crashes, restarts and replays:

1. **Exactly once.** Every input affects the output exactly once: no lost
   articles, no duplicated story events, no half-applied batches, whichever
   process is killed and whenever.
2. **Determinism.** The same input log always produces the same output,
   byte for byte. That's what lets a crashed processor rebuild its state, and
   what lets anyone re-run a past hour and check it.

This document explains how each stage keeps them and how they're tested.

![Architecture](architecture.svg)

## The log is the source of truth

Every stage reads a Kafka topic and writes the next one, and the decisive topic,
`articles.embedded`, has **one partition and is kept forever**. Its offsets give a
total order over every input the processor will ever see, so "the state after
input N" is a well-defined thing. Story events carry the offset of the input that
caused them, the read model is versioned by those offsets, and time travel,
replay and crash recovery are all "re-run the log up to N".

The rule that makes the rest work: **outputs and the progress marker that
produced them are committed atomically**, in whatever system holds the output.

| stage | reads | writes | committed atomically with the output |
|---|---|---|---|
| Embed relay | `articles.raw` | `articles.embedded` | consumed offsets (Kafka transaction) |
| Story Processor | `articles.embedded` | `stories.events`, `articles.late` | next input offset (Kafka transaction) |
| Story Sink | `articles.embedded`, `stories.events` | Postgres read model | consumed offsets (same Postgres transaction) |
| Query API | Postgres | SSE to browsers | event position in the stream id (client resumes from it) |

## Ingestor: at least once, deduplicated by identity

The ingestor publishes to `articles.raw` with an idempotent producer. Article ids
are a hash of the canonicalized URL (`utm_*` and similar tracking parameters
dropped, the rest sorted, fragment, `www.` and trailing slash removed), so the same story fetched twice, from a feed or after
a restart, has the same id. A seen set, rebuilt from `articles.raw` on startup,
keeps restarts from republishing. The only remaining window, a crash between
publishing and recording, can produce a repeat with the **same id**, and the
processor treats a repeated id as a no-op. So from the processor's point of view,
input is exactly once.

## Embed relay: one transaction per batch

```
begin transaction
  embed the batch (gRPC to the Embedder)
  produce the vectors to articles.embedded
  send the consumed offsets of articles.raw to the transaction
commit
```

The relay uses a transactional producer (`transactional.id = embed-relay-0`).
Kafka commits the produced records and the consumer offsets together or not at
all. On any error the relay aborts and exits. On restart, `init_transactions`
fences any zombie instance and aborts its open transaction, and consumption
resumes from the last committed offsets. Downstream consumers read with
`isolation.level=read_committed`, so aborted records are invisible.

Vectors are computed **once**. Nothing downstream re-embeds, because ONNX results
can differ in the last bits with batch composition. Replay and recovery read the
stored vectors, which is what makes the processor's input exactly reproducible.

## Story Processor: epochs, snapshots and silent replay

The processor holds a lot of state: every open story, a MinHash index for
near-duplicates, an HNSW vector index, centroids, the watermark and lineage
cooldowns. Its exactly-once protocol has three parts.

**Epochs.** Inputs are processed in epochs of at most 500. Each epoch's story
events, its late articles and the **next input offset** are written in one Kafka
transaction (`transactional.id = story-processor-0`). Either the whole epoch is
visible downstream, including its progress marker, or none of it is.

**Snapshots after commit.** Every 5,000 inputs or 5 minutes, *after* a commit,
the processor serializes its state (bincode + zstd, about 11 MB for 4,000
articles) with the offset it corresponds to and a fingerprint of the config and
centering. A snapshot never contains uncommitted progress.

**Silent replay on restart.**

1. Read the committed offset `C`: where the outputs end.
2. Load the newest snapshot at or before `C`, at offset `S`. If there's none,
   start from the empty state at offset 0. The log is complete, so this is
   always valid.
3. Re-process `[S, C)` **silently**: rebuild state, discard the outputs, which
   are already committed.
4. Continue from `C`.

Step 3 only works because processing is deterministic, so the rebuilt state is
the state that crashed. That's also why snapshots can be rare while commits are
frequent: **the input log is the write-ahead log**, and a snapshot is only a
shortcut through it. A snapshot whose config fingerprint doesn't match the
running config is refused rather than silently resumed under different rules.

### Where determinism comes from

Nothing in the processor depends on wall-clock time, thread scheduling, hash
iteration order or randomness:

- **Event time only.** The watermark is the maximum event time seen minus 24
  hours of allowed lateness, derived from the log. Housekeeping (closing idle
  stories, evicting their articles) runs on watermark ticks, and lineage checks
  run every 100 inputs, both positions in the log rather than timers.
- **Own HNSW implementation.** Off-the-shelf ANN libraries assign levels from
  an unseeded RNG and build in parallel. Ours draws levels from a fixed-seed RNG
  advanced in insertion order (its state is part of the snapshot) and breaks
  distance ties by key, so the index, and every neighbour list it returns, is a
  pure function of the inserted sequence.
- **Ordered containers and explicit tie-breaks** everywhere a choice is made:
  vote counts, merge candidates, 2-means initialization for splits.
- **Deterministic ids.** Event ids hash `(input offset, sequence in epoch)`, and
  new story ids derive from the article that created them.

### Testing it

- **Restore anywhere** (`snapshot.rs`, property test): for random inputs and a
  random snapshot point, snapshot, restore and continue. Both the events *and the
  final state bytes* must equal an uninterrupted run. Configurations include
  split-heavy and merge-heavy settings.
- **Chaos** (`scripts/chaos/processor.sh`, in CI): the same 20,000-article
  synthetic stream (late arrivals, duplicates and drifting topics) is processed
  twice: once cleanly, once with tiny epochs and frequent snapshots while being
  SIGKILLed at random moments, usually mid-transaction, 50 times. The committed
  `stories.events` and `articles.late` topics of both runs must hash identically.
  Latest local run: 50 kills and 51 starts, 30,750 inputs silently replayed
  during recoveries, and the output was identical: 22,294 story events and 460
  late articles with the same SHA-256 as the clean run, plus a replay of the
  second half matching 11,193 / 11,193.
- **Determinism across builds:** the same fixture gives the same event hash in
  debug and release builds and across runs.

## Story Sink: offsets in the database

The sink writes the read model to Postgres. It doesn't commit offsets to Kafka at
all. Each batch's rows **and the offsets it consumed** go into one Postgres
transaction (table `sink_offsets`), and on startup the sink seeks to the offsets
stored there. A crash before commit loses nothing (the rows weren't written
either), and a crash after commit can't re-apply anything (the offsets moved with
the rows).

Every statement is also idempotent, keyed by event id and article id, so even
applying a whole history twice changes nothing. That's tested directly
(`time_travel_through_merge_and_split_is_idempotent`).

History is never overwritten. Story membership is stored as a validity range
`[from_offset, to_offset)` over input offsets, so a split or merge closes rows
and opens new ones, and the state at any past offset is a query. Time travel in
the UI uses exactly the same queries as the live view.

## Query API: a stream that never gets ahead of the database

The live stream (SSE) is fed by a Postgres `NOTIFY` that the sink sends **inside
the same transaction** as the rows. Notifications are delivered only on commit,
so a client never hears about an event it can't yet query. Each SSE message's id
is the event's position, `<input offset>:<sequence>`. A reconnecting browser sends
`Last-Event-ID` and receives exactly what it missed from the database, in order,
then continues live (verified with 966 events: in order, no gaps, no duplicates).

## Replay: checking the whole chain on real data

Determinism makes a strong check possible: re-run any window of the input log
and compare the result with what the live system committed.

```bash
make replay                 # the whole history
make replay FROM=4500       # a window, warmed up from the nearest live snapshot
```

The replay restores the newest snapshot at or before the window (or starts from
scratch), silently processes up to the window, re-drives it, and compares its
events and late articles **byte for byte, in order** with what's in
`stories.events` and `articles.late`. The report gives both counts, an
order-sensitive hash of each side and the first divergence. The UI's "Re-run this
hour" button does exactly this through the API.

On the live history, with the processor restarted from snapshots several times
along the way:

| replay | warm-up | story events | late | result |
|---|---|---:|---:|---|
| whole history `[0, 5037)` | from scratch | 4,912 / 4,912 | 704 / 704 | identical (6.1 s) |
| `[4500, 5037)` | snapshot @4020 + 404 inputs | 513 / 513 | 1 / 1 | identical (2.3 s) |
| last hour, from the UI | snapshot | 3,047 / 3,047 | 17 / 17 | identical (4.8 s) |
| whole history, one threshold 0.50 → 0.49 | from scratch | 928 / 4,912 match | 702 / 704 | **different**, first at input 413 |

The last row is the control. A comparison that can't fail proves nothing, so a
replay with one similarity threshold nudged by 0.01 must, and does, diverge.

## What isn't covered

- **Wall-clock effects outside the log.** Which articles exist depends on when
  feeds were polled. That's fixed the moment they're written to
  `articles.raw`; everything after is reproducible.
- **Embedding.** Vectors are produced once and stored. Re-embedding the same text
  with a different batch could change low-order bits, which is why nothing does.
- **Single processor partition.** Total order comes from `articles.embedded`
  having one partition. Throughput (about 900 inputs/s on real feeds) is far
  above the news rate. Scaling past one partition would need a deterministic
  merge of partitions, which is out of scope.
- **Listen** (translation and speech) is outside the pipeline. It reads the
  read model and never affects story events.
