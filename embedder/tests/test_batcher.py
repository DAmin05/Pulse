"""Dynamic batcher behavior, using a fake encoder (no model needed)."""

import asyncio
import time

import numpy as np
import pytest

from pulse_embedder.batcher import DynamicBatcher


class FakeEncoder:
    def __init__(self, delay: float = 0.0, fail: bool = False) -> None:
        self.calls: list[int] = []
        self.delay = delay
        self.fail = fail

    def __call__(self, texts: list[str]) -> np.ndarray:
        if self.fail:
            raise RuntimeError("boom")
        self.calls.append(len(texts))
        time.sleep(self.delay)
        # Vector encodes the text so callers can check they got their own results.
        return np.array([[float(t)] for t in texts], dtype=np.float32)


def run(coro):
    return asyncio.run(coro)


def test_merges_concurrent_requests_and_routes_results() -> None:
    async def scenario():
        enc = FakeEncoder()
        b = DynamicBatcher(enc, max_batch=64, max_wait_ms=50)
        await b.start()
        results = await asyncio.gather(*(b.embed([str(i), str(i + 100)]) for i in range(10)))
        await b.stop()
        return enc, results

    enc, results = run(scenario())
    assert enc.calls == [20]  # one model call for all 10 requests
    for i, r in enumerate(results):
        assert r[:, 0].tolist() == [i, i + 100]


def test_respects_max_batch() -> None:
    async def scenario():
        enc = FakeEncoder()
        b = DynamicBatcher(enc, max_batch=4, max_wait_ms=50)
        await b.start()
        await asyncio.gather(*(b.embed([str(i)]) for i in range(10)))
        # A single request larger than max_batch is chunked.
        await b.embed([str(i) for i in range(9)])
        await b.stop()
        return enc

    enc = run(scenario())
    assert all(n <= 4 for n in enc.calls)
    assert sum(enc.calls) == 19


def test_zero_wait_does_not_delay_a_lone_request() -> None:
    async def scenario():
        b = DynamicBatcher(FakeEncoder(), max_batch=64, max_wait_ms=0)
        await b.start()
        t = time.perf_counter()
        await b.embed(["1"])
        elapsed = time.perf_counter() - t
        await b.stop()
        return elapsed

    assert run(scenario()) < 0.02


def test_max_wait_bounds_latency() -> None:
    async def scenario():
        b = DynamicBatcher(FakeEncoder(), max_batch=64, max_wait_ms=30)
        await b.start()
        t = time.perf_counter()
        await b.embed(["1"])  # batch never fills; must flush at the deadline
        elapsed = time.perf_counter() - t
        await b.stop()
        return elapsed

    assert 0.025 < run(scenario()) < 0.15


def test_batches_grow_while_model_is_busy() -> None:
    async def scenario():
        enc = FakeEncoder(delay=0.05)
        b = DynamicBatcher(enc, max_batch=64, max_wait_ms=0)
        await b.start()
        first = asyncio.create_task(b.embed(["0"]))
        await asyncio.sleep(0.01)  # first batch is now running
        await asyncio.gather(first, *(b.embed([str(i)]) for i in range(1, 21)))
        await b.stop()
        return enc

    enc = run(scenario())
    assert enc.calls[0] == 1
    assert enc.calls[1] == 20  # queued during the first call, served together


def test_errors_reach_every_caller() -> None:
    async def scenario():
        b = DynamicBatcher(FakeEncoder(fail=True), max_batch=8, max_wait_ms=10)
        await b.start()
        results = await asyncio.gather(b.embed(["1"]), b.embed(["2"]), return_exceptions=True)
        await b.stop()
        return results

    results = run(scenario())
    assert all(isinstance(r, RuntimeError) for r in results)


def test_rejects_bad_config() -> None:
    with pytest.raises(ValueError):
        DynamicBatcher(FakeEncoder(), max_batch=0, max_wait_ms=1)


def test_sub_batch_plan_bounds_padding() -> None:
    from pulse_embedder.model import plan_sub_batches

    lengths = [10, 200, 12, 11, 190, 15]
    groups = plan_sub_batches(lengths, token_budget=400)
    assert sorted(i for g in groups for i in g) == list(range(6))  # every text exactly once
    for g in groups:
        assert len(g) * max(lengths[i] for i in g) <= 400 or len(g) == 1
    # Short texts share a call; long ones don't drag them into heavy padding.
    assert {0, 2, 3, 5} in [set(g) for g in groups]
    assert plan_sub_batches(lengths, token_budget=0) == [list(range(6))]
    assert plan_sub_batches([], token_budget=100) == []
