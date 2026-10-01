"""gRPC EmbedderService on top of the dynamic batcher."""

from __future__ import annotations

import time

import grpc

from pulse.v1 import embedder_pb2, embedder_pb2_grpc

from . import metrics
from .batcher import DynamicBatcher

# e5 was trained with these prefixes; omitting them degrades retrieval quality.
PREFIXES = {
    embedder_pb2.EMBED_KIND_PASSAGE: "passage: ",
    embedder_pb2.EMBED_KIND_QUERY: "query: ",
}


class EmbedderService(embedder_pb2_grpc.EmbedderServiceServicer):
    def __init__(
        self,
        batcher: DynamicBatcher,
        model_version: str,
        dimensions: int,
        max_texts_per_request: int,
    ) -> None:
        self._batcher = batcher
        self._model_version = model_version
        self._dimensions = dimensions
        self._max_texts = max_texts_per_request

    async def Embed(  # noqa: N802 — gRPC method name
        self, request: embedder_pb2.EmbedRequest, context: grpc.aio.ServicerContext
    ) -> embedder_pb2.EmbedResponse:
        started = time.perf_counter()
        prefix = PREFIXES.get(request.kind)
        if prefix is None:
            metrics.REQUESTS.labels("invalid").inc()
            await context.abort(grpc.StatusCode.INVALID_ARGUMENT, "kind must be PASSAGE or QUERY")
        if not request.texts or len(request.texts) > self._max_texts:
            metrics.REQUESTS.labels("invalid").inc()
            await context.abort(
                grpc.StatusCode.INVALID_ARGUMENT,
                f"texts must contain 1..{self._max_texts} items",
            )

        try:
            vectors = await self._batcher.embed([prefix + t for t in request.texts])
        except Exception as e:  # noqa: BLE001
            metrics.REQUESTS.labels("error").inc()
            await context.abort(grpc.StatusCode.INTERNAL, f"inference failed: {e}")

        metrics.REQUESTS.labels("ok").inc()
        metrics.REQUEST_SECONDS.observe(time.perf_counter() - started)
        return embedder_pb2.EmbedResponse(
            vectors=[embedder_pb2.Vector(values=v) for v in vectors.tolist()],
            model_version=self._model_version,
        )

    async def GetModelInfo(  # noqa: N802
        self, request: embedder_pb2.GetModelInfoRequest, context: grpc.aio.ServicerContext
    ) -> embedder_pb2.GetModelInfoResponse:
        return embedder_pb2.GetModelInfoResponse(
            model_version=self._model_version,
            dimensions=self._dimensions,
            max_batch=self._batcher.max_batch,
            max_wait_ms=round(self._batcher.max_wait * 1000),
        )


def build_server(service: EmbedderService, listen: str) -> tuple[grpc.aio.Server, int]:
    server = grpc.aio.server(
        options=[
            ("grpc.max_receive_message_length", 16 * 1024 * 1024),
            ("grpc.max_send_message_length", 16 * 1024 * 1024),
        ]
    )
    embedder_pb2_grpc.add_EmbedderServiceServicer_to_server(service, server)
    port = server.add_insecure_port(listen)
    return server, port
