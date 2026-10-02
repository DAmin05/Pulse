"""int8 vs fp32: how much does quantization change the vectors?

For each text, the cosine between its fp32 and int8 embeddings; and for the
retrieval that clustering depends on, how many of each text's 10 nearest
neighbours (by fp32) int8 also returns.

  cd embedder && PYTHONPATH=src .venv/bin/python bench/precision.py \\
      --fixture ../data/fixtures/smoke.pulsefx [--limit 2000]
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).parent))
from bench import load_texts  # noqa: E402
from pulse_embedder.model import Encoder  # noqa: E402

K = 10


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", type=Path, required=True)
    parser.add_argument(
        "--model-dir", type=Path, default=Path("../data/models/multilingual-e5-small")
    )
    parser.add_argument("--limit", type=int, default=2000)
    args = parser.parse_args()

    texts = load_texts(args.fixture, args.limit)
    fp32 = Encoder(args.model_dir, "fp32").encode(texts)
    int8 = Encoder(args.model_dir, "int8").encode(texts)

    cos = np.sum(fp32 * int8, axis=1)  # both are L2-normalized
    p = np.percentile(cos, [0, 1, 50])
    print(f"{len(texts)} texts from {args.fixture.name}")
    print(
        f"fp32·int8 cosine: min {p[0]:.4f}  p1 {p[1]:.4f}  "
        f"median {p[2]:.4f}  mean {cos.mean():.4f}"
    )

    def neighbours(v: np.ndarray) -> np.ndarray:
        sims = v @ v.T
        np.fill_diagonal(sims, -np.inf)
        return np.argsort(-sims, axis=1)[:, :K]

    a, b = neighbours(fp32), neighbours(int8)
    overlap = np.mean([len(set(x) & set(y)) / K for x, y in zip(a, b, strict=True)])
    print(f"top-{K} neighbour overlap (int8 vs fp32): {overlap:.3f}")


if __name__ == "__main__":
    main()
