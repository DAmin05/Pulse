"""multilingual-e5-small inference: tokenize → ONNX → mean pool → L2 normalize."""

from __future__ import annotations

import json
from pathlib import Path

import numpy as np
import onnxruntime as ort
from tokenizers import Encoding, Tokenizer

MODEL_NAME = "multilingual-e5-small"
PRECISIONS = ("fp32", "int8")


def plan_sub_batches(lengths: list[int], token_budget: int) -> list[list[int]]:
    """Group indices into model calls with little padding.

    Sorts by token length, then cuts a new group whenever adding the next text
    would make the padded tensor (rows x longest row) exceed `token_budget`.
    A budget of 0 disables this: one group, padded to the longest text.
    """
    if token_budget <= 0:
        return [list(range(len(lengths)))] if lengths else []
    order = sorted(range(len(lengths)), key=lambda i: lengths[i])
    groups: list[list[int]] = []
    current: list[int] = []
    for i in order:
        # Sorted ascending, so lengths[i] is the new longest row.
        if current and (len(current) + 1) * lengths[i] > token_budget:
            groups.append(current)
            current = []
        current.append(i)
    if current:
        groups.append(current)
    return groups


class Encoder:
    """Turns texts into L2-normalized 384-d vectors. Thread-safe for one caller
    at a time; the batcher guarantees that."""

    def __init__(
        self,
        model_dir: Path,
        precision: str = "fp32",
        max_tokens: int = 256,
        intra_op_threads: int = 0,
        token_budget: int = 1024,
    ) -> None:
        if precision not in PRECISIONS:
            raise ValueError(f"precision must be one of {PRECISIONS}, got {precision!r}")
        model_path = model_dir / f"model.{precision}.onnx"
        if not model_path.exists():
            raise FileNotFoundError(f"{model_path} missing — run `make model`")

        self.tokenizer = Tokenizer.from_file(str(model_dir / "tokenizer.json"))
        self.tokenizer.enable_truncation(max_length=max_tokens)
        self.tokenizer.no_padding()  # padded per sub-batch below
        self._pad_id = self.tokenizer.token_to_id("<pad>")
        self.token_budget = token_budget

        opts = ort.SessionOptions()
        opts.graph_optimization_level = ort.GraphOptimizationLevel.ORT_ENABLE_ALL
        if intra_op_threads:
            opts.intra_op_num_threads = intra_op_threads
        self.session = ort.InferenceSession(
            str(model_path), opts, providers=["CPUExecutionProvider"]
        )
        self._input_names = {i.name for i in self.session.get_inputs()}
        self.dimensions = int(self.session.get_outputs()[0].shape[-1])

        revision = "unknown"
        manifest = model_dir / "manifest.json"
        if manifest.exists():
            revision = json.loads(manifest.read_text())["revision"][:7]
        # Everything that changes the vectors is in the version string.
        self.model_version = f"{MODEL_NAME}@{revision}/{precision}/t{max_tokens}"

    def encode(self, texts: list[str]) -> np.ndarray:
        """Returns float32 array of shape (len(texts), dimensions), in input order."""
        out = np.zeros((len(texts), self.dimensions), dtype=np.float32)
        if not texts:
            return out
        encodings = self.tokenizer.encode_batch(texts)
        for group in plan_sub_batches([len(e.ids) for e in encodings], self.token_budget):
            out[group] = self._run([encodings[i] for i in group])
        return out

    def _run(self, encodings: list[Encoding]) -> np.ndarray:
        width = max(len(e.ids) for e in encodings)
        ids = np.full((len(encodings), width), self._pad_id, dtype=np.int64)
        mask = np.zeros((len(encodings), width), dtype=np.int64)
        for row, e in enumerate(encodings):
            ids[row, : len(e.ids)] = e.ids
            mask[row, : len(e.ids)] = 1

        feeds = {"input_ids": ids, "attention_mask": mask}
        if "token_type_ids" in self._input_names:
            feeds["token_type_ids"] = np.zeros_like(ids)
        hidden = self.session.run(None, feeds)[0]  # (n, seq, dim)

        # Mean pooling over real tokens, as e5 was trained.
        m = mask[:, :, None].astype(np.float32)
        pooled = (hidden * m).sum(axis=1) / np.clip(m.sum(axis=1), 1e-9, None)
        norms = np.linalg.norm(pooled, axis=1, keepdims=True)
        return pooled / np.clip(norms, 1e-12, None)
