"""Prometheus metrics, served on PULSE_EMBEDDER_METRICS_PORT (default 9102)."""

from prometheus_client import Counter, Gauge, Histogram

_LATENCY_BUCKETS = (0.001, 0.0025, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0)

QUEUE_DEPTH = Gauge("pulse_embedder_queue_texts", "Texts waiting to be batched")
QUEUE_WAIT = Histogram(
    "pulse_embedder_queue_wait_seconds",
    "Time a request waited before its batch started",
    buckets=_LATENCY_BUCKETS,
)
BATCH_SIZE = Histogram(
    "pulse_embedder_batch_size",
    "Texts per model call",
    buckets=(1, 2, 4, 8, 16, 32, 64, 128, 256),
)
INFERENCE_SECONDS = Histogram(
    "pulse_embedder_inference_seconds",
    "Model time per batch (tokenize + ONNX + pooling)",
    buckets=_LATENCY_BUCKETS,
)
REQUEST_SECONDS = Histogram(
    "pulse_embedder_request_seconds",
    "End-to-end Embed RPC latency",
    buckets=_LATENCY_BUCKETS,
)
TEXTS = Counter("pulse_embedder_texts_total", "Texts embedded")
REQUESTS = Counter("pulse_embedder_requests_total", "Embed RPCs", ["outcome"])
BATCH_ERRORS = Counter("pulse_embedder_batch_errors_total", "Batches that failed in the model")
