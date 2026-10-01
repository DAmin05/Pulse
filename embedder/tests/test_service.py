"""End-to-end over real gRPC. Uses the real model when present, else a fake encoder."""

import asyncio

import grpc
import numpy as np
import pytest

from pulse.v1 import embedder_pb2, embedder_pb2_grpc
from pulse_embedder.batcher import DynamicBatcher
from pulse_embedder.config import DEFAULT_MODEL_DIR
from pulse_embedder.server import EmbedderService, build_server

HAVE_MODEL = (DEFAULT_MODEL_DIR / "model.int8.onnx").exists()


def fake_encode(texts: list[str]) -> np.ndarray:
    v = np.array([[len(t), 1.0] for t in texts], dtype=np.float32)
    return v / np.linalg.norm(v, axis=1, keepdims=True)


async def with_client(encode, version, dims, body):
    batcher = DynamicBatcher(encode, max_batch=16, max_wait_ms=5)
    await batcher.start()
    server, port = build_server(EmbedderService(batcher, version, dims, 64), "127.0.0.1:0")
    await server.start()
    try:
        async with grpc.aio.insecure_channel(f"127.0.0.1:{port}") as ch:
            return await body(embedder_pb2_grpc.EmbedderServiceStub(ch))
    finally:
        await server.stop(None)
        await batcher.stop()


def test_embed_and_validation() -> None:
    async def body(stub):
        info = await stub.GetModelInfo(embedder_pb2.GetModelInfoRequest())
        ok = await stub.Embed(
            embedder_pb2.EmbedRequest(texts=["a", "bbb"], kind=embedder_pb2.EMBED_KIND_PASSAGE)
        )
        errors = []
        for req in (
            embedder_pb2.EmbedRequest(texts=["a"]),  # kind missing
            embedder_pb2.EmbedRequest(kind=embedder_pb2.EMBED_KIND_QUERY),  # no texts
            embedder_pb2.EmbedRequest(texts=["x"] * 65, kind=embedder_pb2.EMBED_KIND_QUERY),
        ):
            with pytest.raises(grpc.aio.AioRpcError) as e:
                await stub.Embed(req)
            errors.append(e.value.code())
        return info, ok, errors

    info, ok, errors = asyncio.run(with_client(fake_encode, "fake@1", 2, body))
    assert info.model_version == "fake@1" and info.dimensions == 2 and info.max_batch == 16
    assert ok.model_version == "fake@1"
    assert len(ok.vectors) == 2
    # The passage prefix was applied before encoding ("passage: a" is 10 chars).
    assert ok.vectors[0].values[0] > ok.vectors[0].values[1]
    assert errors == [grpc.StatusCode.INVALID_ARGUMENT] * 3


@pytest.mark.skipif(not HAVE_MODEL, reason="model not downloaded (make model)")
def test_real_model_is_cross_lingual() -> None:
    from pulse_embedder.model import Encoder

    enc = Encoder(DEFAULT_MODEL_DIR, "int8")

    async def body(stub):
        resp = await stub.Embed(
            embedder_pb2.EmbedRequest(
                texts=[
                    "Strong earthquake hits northern Japan, tsunami warning issued",
                    "Fuerte terremoto sacude el norte de Japón; emiten alerta de tsunami",
                    "Central bank raises interest rates to fight inflation",
                ],
                kind=embedder_pb2.EMBED_KIND_PASSAGE,
            )
        )
        return np.array([v.values for v in resp.vectors])

    v = asyncio.run(with_client(enc.encode, enc.model_version, enc.dimensions, body))
    assert v.shape == (3, 384)
    np.testing.assert_allclose(np.linalg.norm(v, axis=1), 1.0, atol=1e-5)
    same_event, different = v[0] @ v[1], v[0] @ v[2]
    assert same_event > different + 0.05
