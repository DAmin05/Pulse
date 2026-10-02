# Benchmarks

All numbers are from an Apple M3 Pro laptop (CPU only), on real articles from
the RSS feeds unless noted. Each section says how to reproduce it.

## 1. Embedder: dynamic batching on CPU

The Embedder turns `title + summary` into a 384-d vector with
multilingual-e5-small under ONNX Runtime. Requests arrive one article at a time
from the relay, and a dynamic batcher groups them: a batch closes at `max_batch`
texts or `max_wait_ms` after its first request, whichever comes first.

The textbook expectation is that bigger batches mean more throughput. On CPU,
with news articles, **that's false**, and the reason is padding.

![Embedder batching benchmark](bench/embedder.png)

*64 concurrent clients, `max_wait_ms=5`. "Pad to longest" is plain dynamic
batching; "length-bucketed" sorts each batch by token count and runs it as
sub-batches of at most 1,024 padded tokens.*

| precision | batching | max_batch 1 | max_batch 64 | p99 at 64 |
|---|---|---:|---:|---:|
| fp32 | pad to longest | 99 texts/s | **64 texts/s** (−35%) | 1,352 ms |
| fp32 | length-bucketed | 99 texts/s | **107 texts/s** (+8%) | 727 ms |
| int8 | pad to longest | 190 texts/s | **149 texts/s** (−22%) | 644 ms |
| int8 | length-bucketed | 188 texts/s | **213 texts/s** (+13%) | 546 ms |

**Why.** A transformer's cost grows with sequence length, and a batch is padded to
its longest member. Article lengths vary a lot: a headline with no summary is ~20
tokens, a long summary hits the 256-token cap. In a batch of 64, nearly every
batch contains one long text, so most of the compute goes to padding. CPUs don't
have the idle parallel units that make padding nearly free on a GPU.

**Fix.** Sort each batch by length and split it into sub-batches under a padded
token budget. Short texts run together with little padding, long ones run in
small groups. The batcher still collects 64 at a time (amortizing per-call
overhead), but no compute is wasted. At 64 clients that turns a 35% loss into an
8% gain on fp32, halves p99 latency, and gives int8 its best throughput.

**At light load, waiting is pure cost.** With 4 clients, raising `max_wait_ms`
from 0 to 20 ms cuts fp32 throughput from 96 to 60 texts/s (int8: 196 → 99): the
batcher holds requests for company that never arrives. Keep `max_wait_ms` well
below the gap between arrivals.

**Defaults:** int8, `max_batch=64`, `max_wait_ms=5`, token budget 1,024.

Method: closed-loop clients each send one text and wait for the answer, 3-second
runs per point, 2,000 real articles. p99 from short runs is noisy (see the int8
p99 dip at batch 16 in the chart), so read trends rather than single points.
Full grid of 144 runs: [bench/embedder.md](bench/embedder.md).

```bash
make bench FIXTURE=data/fixtures/<file>.pulsefx   # regenerates bench/embedder.md and .png
```

## 2. int8 vs fp32: what quantization costs

int8 doubles throughput. What does it do to the vectors?

| measure (2,000 articles) | value |
|---|---:|
| cosine between fp32 and int8 vectors of the same text: median | 0.9967 |
| … 1st percentile | 0.9941 |
| … worst | 0.9917 |
| top-10 nearest neighbours shared with fp32 | 85.3% |

The vectors barely move (median cosine 0.997), but nearest-neighbour lists do
change. About 1.5 of each article's 10 neighbours differ, because news contains
many near-ties (wire copies, follow-ups). That matters less than it sounds:
clustering thresholds were tuned on int8 vectors, the vote needs agreement among
neighbours plus a centroid fit, and every component reads the same stored
vectors, so there's no fp32/int8 mixing.

```bash
cd embedder && PYTHONPATH=src .venv/bin/python bench/precision.py --fixture ../data/fixtures/<file>.pulsefx
```

## 3. Story Processor: clustering throughput and recall

Single-threaded, deterministic, end to end per article: MinHash near-duplicate
check, HNSW search, vote, centroid update, lineage checks every 100 inputs.

| fixture | articles | stories with ≥2 sources | cross-lingual | HNSW recall@10 (ef 64) | throughput |
|---|---:|---:|---:|---:|---:|
| RSS, 132 feeds, 72 h | 3,989 | 302 | 118 | 0.996 | ~900 articles/s |
| GDELT translingual, 4 h | 49,220 | 5,477 | 1,236 | 0.978 | ~700 articles/s |

Recall is measured against brute-force search on the same vectors. For
comparison, the live feeds delivered about 10 new articles a minute in steady
state (metrics panel), and the cold-start backlog of ~4,000 articles takes the
processor about 5 seconds, so a single processor has ample headroom.

```bash
make cluster-eval EMBEDDED=data/fixtures/<file>.pulseem
make ann-recall EMBEDDED=data/fixtures/<file>.pulseem
```

## 4. Replay speed

Replaying is processing with comparison, from a snapshot or from scratch.

| replay | inputs re-driven | time |
|---|---:|---:|
| whole live history, from scratch | 5,037 | 6.1 s |
| window, from snapshot @4020 | 404 + 513 | 2.3 s |
| last hour, from the UI | 2,548 | 4.8 s |

## 5. Live pipeline

Observed on the live pipeline (RSS, 134 feeds), from the metrics panel: the
median time from fetch to the embedded article being committed was 0.44 s, and
p95 inference latency was 87.5 ms, with every stage's consumer lag at zero.
