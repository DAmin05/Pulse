"""Dynamic batching benchmark: batch size and wait time vs throughput and latency.

Closed-loop clients, each sending one text per request (the worst case for
batching), drive the real DynamicBatcher + Encoder in-process, so the numbers
isolate batching and inference from network overhead. Texts are real articles
from a fixture file.

Grid: precision x token_budget x concurrency x max_batch x max_wait_ms.
token_budget 0 pads each batch to its longest text; > 0 sorts by length and
splits into sub-batches of at most that many padded tokens. Writes a CSV, a
markdown summary and a chart comparing the two under heavy load.

Usage:
  python bench/bench.py --fixture ../data/fixtures/smoke.pulsefx [--duration 3]
"""

from __future__ import annotations

import argparse
import asyncio
import csv
import itertools
import statistics
import time
from dataclasses import asdict, dataclass
from pathlib import Path

import numpy as np
from google.protobuf.internal.decoder import _DecodeVarint32

from pulse.v1 import article_pb2
from pulse_embedder.batcher import DynamicBatcher
from pulse_embedder.config import DEFAULT_MODEL_DIR
from pulse_embedder.model import Encoder

REPO = Path(__file__).resolve().parents[2]
MAGIC = b"PULSEFX1"


@dataclass
class Result:
    precision: str
    token_budget: int
    concurrency: int
    max_batch: int
    max_wait_ms: int
    throughput: float  # texts / second
    p50_ms: float
    p99_ms: float
    mean_batch: float


def load_texts(path: Path, limit: int = 2000) -> list[str]:
    data = path.read_bytes()
    if not data.startswith(MAGIC):
        raise SystemExit(f"{path} is not a Pulse fixture")
    pos, texts = len(MAGIC), []
    while pos < len(data) and len(texts) < limit:
        size, pos = _DecodeVarint32(data, pos)
        a = article_pb2.Article.FromString(data[pos : pos + size])
        pos += size
        texts.append("passage: " + (f"{a.title}\n{a.summary}" if a.summary else a.title))
    return texts


async def run_config(
    encoder: Encoder,
    texts: list[str],
    concurrency: int,
    max_batch: int,
    max_wait_ms: int,
    duration: float,
    warmup: float,
) -> tuple[float, list[float], list[int]]:
    batch_sizes: list[int] = []
    measuring = False

    def encode(batch: list[str]) -> np.ndarray:
        if measuring:
            batch_sizes.append(len(batch))
        return encoder.encode(batch)

    batcher = DynamicBatcher(encode, max_batch, max_wait_ms)
    await batcher.start()
    latencies: list[float] = []
    stop = False

    async def client(i: int) -> None:
        k = i
        while not stop:
            t = time.perf_counter()
            await batcher.embed([texts[k % len(texts)]])
            if measuring:
                latencies.append(time.perf_counter() - t)
            k += concurrency

    tasks = [asyncio.create_task(client(i)) for i in range(concurrency)]
    await asyncio.sleep(warmup)
    measuring = True
    started = time.perf_counter()
    await asyncio.sleep(duration)
    measuring = False
    elapsed = time.perf_counter() - started
    stop = True
    await asyncio.gather(*tasks)
    await batcher.stop()
    return len(latencies) / elapsed, latencies, batch_sizes


def percentile(values: list[float], q: float) -> float:
    return float(np.percentile(values, q)) if values else float("nan")


def plot(results: list[Result], out: Path, load: int, wait_ms: int) -> None:
    """Naive padding vs length-bucketed batching, per precision, at one load."""
    import matplotlib

    matplotlib.use("Agg")
    import matplotlib.pyplot as plt

    budgets = sorted({r.token_budget for r in results})
    precisions = list(dict.fromkeys(r.precision for r in results))
    # Validated categorical slots 1-2 (dataviz reference palette) + marker shape.
    styles = [("#2a78d6", "o"), ("#eb6834", "s")]
    names = {0: "pad to longest"}
    ink, muted, grid, surface = "#0b0b0b", "#898781", "#e1e0d9", "#fcfcfb"
    rows = [("throughput", "Throughput (texts/s)"), ("p99_ms", "p99 latency (ms)")]

    fig, axes = plt.subplots(
        2, len(precisions), figsize=(5.4 * len(precisions), 7.2), sharex=True, squeeze=False
    )
    fig.patch.set_facecolor(surface)
    for col, precision in enumerate(precisions):
        for row, (field, label) in enumerate(rows):
            ax = axes[row][col]
            ax.set_facecolor(surface)
            for budget, (color, marker) in zip(budgets, styles, strict=False):
                pts = sorted(
                    (r.max_batch, getattr(r, field))
                    for r in results
                    if r.precision == precision
                    and r.token_budget == budget
                    and r.concurrency == load
                    and r.max_wait_ms == wait_ms
                )
                if not pts:
                    continue
                name = names.get(budget, f"length-bucketed ({budget} tok)")
                xs, ys = zip(*pts, strict=True)
                ax.plot(
                    xs,
                    ys,
                    color=color,
                    linewidth=2,
                    marker=marker,
                    markersize=8,
                    markeredgecolor=surface,
                    markeredgewidth=2,
                    label=name,
                )
                # Direct label at the line end, in ink rather than series color.
                ax.annotate(
                    name.split(" (")[0],
                    (xs[-1], ys[-1]),
                    xytext=(6, 0),
                    textcoords="offset points",
                    va="center",
                    fontsize=8,
                    color=ink,
                )
            ax.set_xscale("log", base=2)
            ax.set_xticks([1, 4, 8, 16, 32, 64], labels=["1", "4", "8", "16", "32", "64"])
            ax.set_xlim(0.8, 160)
            ax.set_ylim(bottom=0)
            ax.grid(axis="y", color=grid, linewidth=0.8)
            ax.tick_params(colors=muted, labelsize=9)
            for side in ("top", "right"):
                ax.spines[side].set_visible(False)
            for side in ("left", "bottom"):
                ax.spines[side].set_color(grid)
            if col == 0:
                ax.set_ylabel(label, color=ink, fontsize=10)
            if row == 0:
                ax.set_title(precision, color=ink, fontsize=11, loc="left")
            if row == len(rows) - 1:
                ax.set_xlabel("max_batch (texts per batch)", color=ink, fontsize=10)
    axes[0][0].legend(frameon=False, fontsize=9, labelcolor=ink, loc="lower left")
    fig.suptitle(
        f"Embedder batching on CPU: {load} concurrent clients, max_wait_ms={wait_ms}",
        color=ink,
        fontsize=12,
        x=0.02,
        ha="left",
    )
    fig.tight_layout()
    fig.savefig(out, dpi=150, facecolor=surface)


def write_summary(results: list[Result], path: Path, machine: str) -> None:
    lines = [
        "# Embedder benchmark",
        "",
        f"Machine: {machine}. Closed-loop clients send one text per request to the in-process",
        "DynamicBatcher + ONNX encoder (no network). Texts are real articles (title + summary).",
        "",
        "| precision | token budget | clients | max_batch | max_wait_ms "
        "| texts/s | p50 ms | p99 ms | mean batch |",
        "|---|---:|---:|---:|---:|---:|---:|---:|---:|",
    ]
    for r in results:
        lines.append(
            f"| {r.precision} | {r.token_budget or 'off'} | {r.concurrency} "
            f"| {r.max_batch} | {r.max_wait_ms} | "
            f"{r.throughput:.0f} | {r.p50_ms:.1f} | {r.p99_ms:.1f} | {r.mean_batch:.1f} |"
        )
    path.write_text("\n".join(lines) + "\n")


async def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument("--precisions", default="fp32,int8")
    parser.add_argument("--loads", default="4,64", help="concurrent clients")
    parser.add_argument("--batches", default="1,4,8,16,32,64")
    parser.add_argument("--waits", default="0,5,20")
    parser.add_argument("--budgets", default="0,1024", help="0 = naive padding")
    parser.add_argument("--duration", type=float, default=3.0)
    parser.add_argument("--warmup", type=float, default=0.5)
    parser.add_argument("--chart-wait", type=int, default=5, help="max_wait_ms shown in chart")
    parser.add_argument("--chart-load", type=int, default=64, help="clients shown in chart")
    parser.add_argument("--out", type=Path, default=REPO / "data" / "bench")
    parser.add_argument("--docs", type=Path, default=REPO / "docs" / "bench")
    args = parser.parse_args()

    ints = lambda s: [int(x) for x in s.split(",")]  # noqa: E731
    loads, batches, waits = ints(args.loads), ints(args.batches), ints(args.waits)
    budgets = ints(args.budgets)
    texts = load_texts(args.fixture)
    print(f"{len(texts)} texts, median {statistics.median(len(t) for t in texts)} chars")

    results: list[Result] = []
    for precision in args.precisions.split(","):
        encoder = Encoder(DEFAULT_MODEL_DIR, precision)
        encoder.encode(texts[:8])  # warm up the graph
        for budget, load, max_batch, wait in itertools.product(budgets, loads, batches, waits):
            encoder.token_budget = budget
            tput, lat, sizes = await run_config(
                encoder, texts, load, max_batch, wait, args.duration, args.warmup
            )
            r = Result(
                precision,
                budget,
                load,
                max_batch,
                wait,
                tput,
                percentile(lat, 50) * 1000,
                percentile(lat, 99) * 1000,
                statistics.mean(sizes) if sizes else 0.0,
            )
            results.append(r)
            print(
                f"{precision} budget={budget:<5} c={load:<3} batch={max_batch:<3} wait={wait:<3} "
                f"{tput:7.0f} texts/s  p50 {r.p50_ms:7.1f} ms  p99 {r.p99_ms:7.1f} ms  "
                f"mean batch {r.mean_batch:5.1f}"
            )

    args.out.mkdir(parents=True, exist_ok=True)
    args.docs.mkdir(parents=True, exist_ok=True)
    with (args.out / "embedder.csv").open("w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=list(asdict(results[0])))
        w.writeheader()
        w.writerows(asdict(r) for r in results)

    import platform
    import subprocess

    try:
        cpu = subprocess.check_output(["sysctl", "-n", "machdep.cpu.brand_string"], text=True)
    except (OSError, subprocess.CalledProcessError):
        cpu = platform.processor()
    write_summary(results, args.docs / "embedder.md", cpu.strip())
    plot(results, args.docs / "embedder.png", args.chart_load, args.chart_wait)
    print(f"wrote {args.out / 'embedder.csv'} and {args.docs}/embedder.{{md,png}}")


if __name__ == "__main__":
    asyncio.run(main())
