import { CircleAlert, CircleCheck, TriangleAlert, X } from "lucide-react";

import { usePipeline, useStats } from "../api/hooks";
import type { PipelineMetric, PipelineStage } from "../api/types";
import { count, metric } from "../lib/format";
import { useApp } from "../live/store";
import { Sparkline } from "./Sparkline";

const TILE_ORDER = [
  "embed_rate",
  "ingest_rate",
  "process_rate",
  "events_rate",
  "pipeline_latency",
  "embed_p95",
  "embed_batch",
  "dedup_rate",
  "late_rate",
  "watermark_lag",
];

export function MetricsPanel() {
  const open = useApp((s) => s.metricsOpen);
  const toggle = useApp((s) => s.toggleMetrics);
  const pipeline = usePipeline(open);
  const stats = useStats();
  if (!open) return null;

  const metrics = new Map((pipeline.data?.metrics ?? []).map((m) => [m.key, m]));
  const totals = stats.data?.totals;

  return (
    <aside className="metrics" aria-labelledby="metrics-title">
      <div className="metrics__head">
        <div>
          <h2 id="metrics-title" className="metrics__title">
            Pipeline
          </h2>
          <p className="metrics__sub">Last 30 minutes · refreshes every 5s</p>
        </div>
        <button className="icon-btn" onClick={() => toggle(false)} aria-label="Close pipeline metrics">
          <X size={18} />
        </button>
      </div>

      <section className="flow" aria-label="Consumer lag by stage">
        <FlowStep name="Ingestor" note="RSS & GDELT" />
        <FlowLink topic="articles.raw" />
        <FlowStep name="Embed relay" stage={pipeline.data?.stages.find((s) => s.group === "embed-relay")} />
        <FlowLink topic="articles.embedded" />
        <FlowStep name="Story processor" stage={pipeline.data?.stages.find((s) => s.group === "story-processor")} />
        <FlowLink topic="stories.events" />
        <FlowStep name="Sink → Postgres" stage={pipeline.data?.stages.find((s) => s.group === "postgres")} />
      </section>

      {pipeline.isError && <p className="metrics__error">Metrics unavailable: is Prometheus running (make up)?</p>}

      <section className="tiles">
        {TILE_ORDER.map((key) => {
          const m = metrics.get(key);
          return m ? <Tile key={key} metric={m} /> : <TileSkeleton key={key} />;
        })}
      </section>

      {totals && (
        <section className="totals">
          <h3 className="section-title">Since the start</h3>
          <dl>
            <div>
              <dt>Articles</dt>
              <dd className="num">{count(totals.articles)}</dd>
            </div>
            <div>
              <dt>Stories open</dt>
              <dd className="num">{count(totals.stories_open)}</dd>
            </div>
            <div>
              <dt>Syndicated copies</dt>
              <dd className="num">
                {count(totals.duplicates)}{" "}
                <span className="muted">({((100 * totals.duplicates) / Math.max(1, totals.articles)).toFixed(1)}%)</span>
              </dd>
            </div>
            <div>
              <dt>Late arrivals</dt>
              <dd className="num">{count(totals.late_articles)}</dd>
            </div>
            <div>
              <dt>Merges · splits</dt>
              <dd className="num">
                {count(totals.merges)} · {count(totals.splits)}
              </dd>
            </div>
            <div>
              <dt>Sources · languages</dt>
              <dd className="num">
                {count(totals.sources)} · {totals.languages}
              </dd>
            </div>
          </dl>
        </section>
      )}
    </aside>
  );
}

function lagStatus(lag: number | null | undefined) {
  if (lag === null || lag === undefined) return { icon: CircleAlert, label: "Unknown", tone: "muted" } as const;
  if (lag <= 5) return { icon: CircleCheck, label: "Caught up", tone: "good" } as const;
  if (lag <= 500) return { icon: TriangleAlert, label: `${count(lag)} behind`, tone: "warning" } as const;
  return { icon: CircleAlert, label: `${count(lag)} behind`, tone: "critical" } as const;
}

function FlowStep({ name, note, stage }: { name: string; note?: string; stage?: PipelineStage }) {
  const status = stage ? lagStatus(stage.lag) : null;
  const Icon = status?.icon;
  return (
    <div className="flow__step">
      <span className="flow__name">{name}</span>
      {status && Icon ? (
        <span className={`flow__status flow__status--${status.tone}`}>
          <Icon size={13} aria-hidden />
          {status.label}
        </span>
      ) : (
        <span className="flow__note">{note}</span>
      )}
    </div>
  );
}

function FlowLink({ topic }: { topic: string }) {
  return (
    <div className="flow__link" aria-hidden>
      <span className="flow__arrow" />
      <span className="flow__topic">{topic}</span>
    </div>
  );
}

function Tile({ metric: m }: { metric: PipelineMetric }) {
  const values = m.series.map((p) => p[1]);
  return (
    <div className="tile">
      <p className="tile__label">{m.label}</p>
      <p className="tile__value">
        {metric(m.value)}
        <span className="tile__unit">{m.unit}</span>
      </p>
      <Sparkline values={values} width={170} height={30} label={`${m.label}, last 30 minutes`} />
    </div>
  );
}

function TileSkeleton() {
  return (
    <div className="tile tile--skeleton" aria-hidden>
      <span />
      <span />
      <span />
    </div>
  );
}
