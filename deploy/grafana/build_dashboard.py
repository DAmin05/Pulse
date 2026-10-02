"""Generates provisioning/dashboards/pulse.json (run after editing panels).

  python3 deploy/grafana/build_dashboard.py
"""

import json
from pathlib import Path

DS = {"type": "prometheus", "uid": "prometheus"}
panels = []
y = 0


def row(title):
    global y
    panels.append({"type": "row", "title": title, "collapsed": False, "gridPos": {"h": 1, "w": 24, "x": 0, "y": y}})
    y += 1


def ts(title, targets, unit="short", x=0, w=8, h=7, desc=None, stack=False):
    panels.append({
        "type": "timeseries",
        "title": title,
        "description": desc or "",
        "datasource": DS,
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "fieldConfig": {
            "defaults": {
                "unit": unit,
                "custom": {"lineWidth": 2, "fillOpacity": 12, "stacking": {"mode": "normal" if stack else "none"}},
            },
            "overrides": [],
        },
        "options": {"legend": {"displayMode": "list", "placement": "bottom"}, "tooltip": {"mode": "multi"}},
        "targets": [{"datasource": DS, "expr": e, "legendFormat": l, "refId": chr(65 + i)} for i, (e, l) in enumerate(targets)],
    })


def stat(title, expr, unit="short", x=0, w=4, h=4, desc=None):
    panels.append({
        "type": "stat",
        "title": title,
        "description": desc or "",
        "datasource": DS,
        "gridPos": {"h": h, "w": w, "x": x, "y": y},
        "fieldConfig": {"defaults": {"unit": unit, "color": {"mode": "thresholds"},
                                     "thresholds": {"mode": "absolute", "steps": [{"color": "green", "value": None}]}}},
        "options": {"reduceOptions": {"calcs": ["lastNotNull"]}, "graphMode": "area", "colorMode": "value"},
        "targets": [{"datasource": DS, "expr": expr, "refId": "A"}],
    })


def advance(h):
    global y
    y += h


row("Overview")
stat("Articles ingested / min", "sum(rate(pulse_ingestor_published_total[5m])) * 60", x=0)
stat("Embedded / s", "sum(rate(pulse_embedder_texts_total[2m]))", x=4)
stat("Story events / min", "sum(rate(pulse_processor_events_total[5m])) * 60", x=8)
stat("Open stories", "max(pulse_processor_open_stories)", x=12)
stat("Watermark behind now", "max(pulse_processor_watermark_lag_seconds)", unit="s", x=16,
     desc="Event time of the newest input minus 24 h of allowed lateness, relative to now.")
stat("Sink lag (records)", "sum(pulse_sink_lag_records)", x=20)
advance(4)

row("Ingest")
ts("Articles published / min", [("sum(rate(pulse_ingestor_published_total[5m])) * 60", "published")])
ts("Feed polls by outcome / min", [("sum by (outcome) (rate(pulse_ingestor_polls_total[5m])) * 60", "{{outcome}}")], x=8, stack=True)
ts("Poll duration p95", [("histogram_quantile(0.95, sum by (le) (rate(pulse_ingestor_poll_duration_seconds_bucket[5m])))", "p95")], unit="s", x=16)
advance(7)

row("Embed")
ts("Texts embedded / s", [("sum(rate(pulse_embedder_texts_total[2m]))", "texts/s")])
ts("Inference latency", [
    ("histogram_quantile(0.5, sum by (le) (rate(pulse_embedder_inference_seconds_bucket[5m])))", "p50"),
    ("histogram_quantile(0.95, sum by (le) (rate(pulse_embedder_inference_seconds_bucket[5m])))", "p95"),
], unit="s", x=8)
ts("Fetch → embedded (relay)", [
    ("histogram_quantile(0.5, sum by (le) (rate(pulse_relay_ingest_to_embedded_seconds_bucket[5m])))", "p50"),
    ("histogram_quantile(0.95, sum by (le) (rate(pulse_relay_ingest_to_embedded_seconds_bucket[5m])))", "p95"),
], unit="s", x=16, desc="From the ingestor fetching an article to its vector being committed to articles.embedded.")
advance(7)
ts("Mean batch size", [("sum(rate(pulse_embedder_batch_size_sum[5m])) / sum(rate(pulse_embedder_batch_size_count[5m]))", "texts per batch")])
ts("Queue wait p95", [("histogram_quantile(0.95, sum by (le) (rate(pulse_embedder_queue_wait_seconds_bucket[5m])))", "p95")], unit="s", x=8)
ts("Relay transaction commit p95", [("histogram_quantile(0.95, sum by (le) (rate(pulse_relay_commit_seconds_bucket[5m])))", "p95")], unit="s", x=16)
advance(7)

row("Cluster")
ts("Inputs by outcome / min", [("sum by (outcome) (rate(pulse_processor_inputs_total[5m])) * 60", "{{outcome}}")], stack=True)
ts("Story events by kind / min", [("sum by (kind) (rate(pulse_processor_events_total[5m])) * 60", "{{kind}}")], x=8, stack=True)
ts("State size", [
    ("max(pulse_processor_open_stories)", "open stories"),
    ("max(pulse_processor_articles)", "articles in memory"),
    ("max(pulse_processor_index_tombstones)", "HNSW tombstones"),
], x=16)
advance(7)
ts("Epoch and commit p95", [
    ("histogram_quantile(0.95, sum by (le) (rate(pulse_processor_epoch_seconds_bucket[5m])))", "epoch"),
    ("histogram_quantile(0.95, sum by (le) (rate(pulse_processor_commit_seconds_bucket[5m])))", "transaction commit"),
], unit="s")
ts("Snapshots", [("max(pulse_processor_snapshot_bytes)", "snapshot size")], unit="bytes", x=8)
ts("Watermark lag", [("max(pulse_processor_watermark_lag_seconds) / 3600", "hours behind now")], unit="h", x=16)
advance(7)

row("Serve")
ts("Sink lag by topic", [("sum by (topic) (pulse_sink_lag_records)", "{{topic}}")])
ts("API latency p95 by route", [("histogram_quantile(0.95, sum by (le, route) (rate(pulse_api_request_seconds_bucket[5m])))", "{{route}}")], unit="s", x=8)
ts("Live SSE clients", [("sum(pulse_api_live_clients)", "clients")], x=16)
advance(7)

dashboard = {
    "uid": "pulse-pipeline",
    "title": "Pulse pipeline",
    "tags": ["pulse"],
    "timezone": "browser",
    "schemaVersion": 39,
    "refresh": "10s",
    "time": {"from": "now-1h", "to": "now"},
    "panels": [dict(p, id=i + 1) for i, p in enumerate(panels)],
}
out = Path(__file__).parent / "provisioning" / "dashboards" / "pulse.json"
out.write_text(json.dumps(dashboard, indent=2) + "\n")
print(f"wrote {out} ({len(panels)} panels)")
