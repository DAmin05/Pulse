"""Dynamic batching: merge concurrent requests into one model call.

A batch closes when it holds `max_batch` texts or `max_wait_ms` has passed since
its first request arrived, whichever comes first. Inference runs on a single
worker thread (ONNX Runtime parallelizes inside each call and releases the GIL),
so while one batch runs the next one fills up. Under load, batches grow on their
own and `max_wait_ms` stops mattering; under light load, it caps added latency.
"""

from __future__ import annotations

import asyncio
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field

import numpy as np

from . import metrics

EncodeFn = Callable[[list[str]], np.ndarray]


@dataclass
class _Request:
    texts: list[str]
    future: asyncio.Future[np.ndarray]
    enqueued_at: float = field(default_factory=time.perf_counter)


class DynamicBatcher:
    def __init__(self, encode: EncodeFn, max_batch: int, max_wait_ms: float) -> None:
        if max_batch < 1:
            raise ValueError("max_batch must be >= 1")
        self._encode = encode
        self.max_batch = max_batch
        self.max_wait = max_wait_ms / 1000
        self._queue: asyncio.Queue[_Request] = asyncio.Queue()
        self._executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="inference")
        self._task: asyncio.Task[None] | None = None

    async def start(self) -> None:
        self._task = asyncio.create_task(self._run(), name="batcher")

    async def stop(self) -> None:
        if self._task:
            self._task.cancel()
            try:
                await self._task
            except asyncio.CancelledError:
                pass
        self._executor.shutdown(wait=True)

    async def embed(self, texts: list[str]) -> np.ndarray:
        if not texts:
            return np.zeros((0, 0), dtype=np.float32)
        future = asyncio.get_running_loop().create_future()
        self._queue.put_nowait(_Request(texts, future))
        metrics.QUEUE_DEPTH.inc(len(texts))
        return await future

    async def _collect(self) -> list[_Request]:
        first = await self._queue.get()
        batch, size = [first], len(first.texts)
        deadline = first.enqueued_at + self.max_wait
        while size < self.max_batch:
            remaining = deadline - time.perf_counter()
            try:
                if remaining <= 0:
                    req = self._queue.get_nowait()  # take only what's already waiting
                else:
                    req = await asyncio.wait_for(self._queue.get(), remaining)
            except (asyncio.QueueEmpty, TimeoutError):
                break
            batch.append(req)
            size += len(req.texts)
        return batch

    async def _run(self) -> None:
        loop = asyncio.get_running_loop()
        while True:
            batch = await self._collect()
            texts = [t for r in batch for t in r.texts]
            metrics.QUEUE_DEPTH.dec(len(texts))
            started = time.perf_counter()
            for r in batch:
                metrics.QUEUE_WAIT.observe(started - r.enqueued_at)
            try:
                vectors = await loop.run_in_executor(self._executor, self._encode_chunked, texts)
            except Exception as e:  # noqa: BLE001 — fail every caller in the batch
                metrics.BATCH_ERRORS.inc()
                for r in batch:
                    if not r.future.done():
                        r.future.set_exception(e)
                continue
            metrics.INFERENCE_SECONDS.observe(time.perf_counter() - started)
            metrics.TEXTS.inc(len(texts))

            offset = 0
            for r in batch:
                n = len(r.texts)
                if not r.future.done():  # caller may have gone away
                    r.future.set_result(vectors[offset : offset + n])
                offset += n

    def _encode_chunked(self, texts: list[str]) -> np.ndarray:
        # One oversized request can push a batch past max_batch; keep model calls bounded.
        chunks = [texts[i : i + self.max_batch] for i in range(0, len(texts), self.max_batch)]
        for chunk in chunks:
            metrics.BATCH_SIZE.observe(len(chunk))
        return np.concatenate([self._encode(c) for c in chunks])
