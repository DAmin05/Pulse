"""Run the embedding service: python -m pulse_embedder"""

from __future__ import annotations

import asyncio
import logging
import signal

from prometheus_client import start_http_server

from .batcher import DynamicBatcher
from .config import Settings
from .model import Encoder
from .server import EmbedderService, build_server

log = logging.getLogger("pulse_embedder")


async def serve(settings: Settings) -> None:
    encoder = Encoder(
        settings.model_dir,
        settings.precision,
        settings.max_tokens,
        settings.intra_op_threads,
        settings.token_budget,
    )
    encoder.encode(["passage: warmup"])  # first call pays graph initialization
    log.info(
        "model %s loaded (%d dims); max_batch=%d max_wait_ms=%d token_budget=%d",
        encoder.model_version,
        encoder.dimensions,
        settings.max_batch,
        settings.max_wait_ms,
        settings.token_budget,
    )

    batcher = DynamicBatcher(encoder.encode, settings.max_batch, settings.max_wait_ms)
    await batcher.start()
    service = EmbedderService(
        batcher, encoder.model_version, encoder.dimensions, settings.max_texts_per_request
    )
    server, port = build_server(service, settings.listen)
    await server.start()
    start_http_server(settings.metrics_port)
    log.info("serving gRPC on %s, metrics on :%d", settings.listen, settings.metrics_port)

    stop = asyncio.Event()
    loop = asyncio.get_running_loop()
    for sig in (signal.SIGINT, signal.SIGTERM):
        loop.add_signal_handler(sig, stop.set)
    await stop.wait()

    log.info("shutting down")
    await server.stop(grace=5)  # finish in-flight requests
    await batcher.stop()


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
    asyncio.run(serve(Settings.from_env()))


if __name__ == "__main__":
    main()
