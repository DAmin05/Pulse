"""Settings from environment variables (see .env.example)."""

from __future__ import annotations

import os
from dataclasses import dataclass
from pathlib import Path

DEFAULT_MODEL_DIR = (
    Path(__file__).resolve().parents[3] / "data" / "models" / "multilingual-e5-small"
)


def _int(name: str, default: int) -> int:
    return int(os.environ.get(name, default))


@dataclass(frozen=True)
class Settings:
    listen: str = "0.0.0.0:50061"
    model_dir: Path = DEFAULT_MODEL_DIR
    # fp32 | int8. int8 is ~2x faster; median cosine 0.997 vs fp32 (bench/precision.py).
    precision: str = "int8"
    max_batch: int = 64
    max_wait_ms: int = 5
    max_tokens: int = 256
    token_budget: int = 1024  # padded tokens per model call; 0 = pad whole batch
    intra_op_threads: int = 0  # 0 = ONNX Runtime default (all physical cores)
    max_texts_per_request: int = 256
    metrics_port: int = 9102

    @classmethod
    def from_env(cls) -> Settings:
        return cls(
            listen=os.environ.get("PULSE_EMBEDDER_LISTEN", cls.listen),
            model_dir=Path(os.environ.get("PULSE_EMBEDDER_MODEL_DIR", DEFAULT_MODEL_DIR)),
            precision=os.environ.get("PULSE_EMBEDDER_PRECISION", cls.precision),
            max_batch=_int("PULSE_EMBEDDER_MAX_BATCH", cls.max_batch),
            max_wait_ms=_int("PULSE_EMBEDDER_MAX_WAIT_MS", cls.max_wait_ms),
            max_tokens=_int("PULSE_EMBEDDER_MAX_TOKENS", cls.max_tokens),
            token_budget=_int("PULSE_EMBEDDER_TOKEN_BUDGET", cls.token_budget),
            intra_op_threads=_int("PULSE_EMBEDDER_THREADS", cls.intra_op_threads),
            max_texts_per_request=_int("PULSE_EMBEDDER_MAX_TEXTS", cls.max_texts_per_request),
            metrics_port=_int("PULSE_EMBEDDER_METRICS_PORT", cls.metrics_port),
        )
