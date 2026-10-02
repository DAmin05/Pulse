import { Pause, Play, Radio, ShieldCheck } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { useTimeline } from "../api/hooks";
import type { TimelineBucket } from "../api/types";
import { clockTime, count, plural } from "../lib/format";
import { useApp } from "../live/store";
import { ReplayDialog } from "./ReplayDialog";

const PLAY_STEP_MS = 650;
const HEIGHT = 60;

interface Window {
  from: number;
  to: number;
  label: string;
}

export function Timeline() {
  const timeline = useTimeline();
  const view = useApp((s) => s.view);
  const { goTo, goLive } = useApp.getState();
  const buckets = useMemo(() => timeline.data?.buckets ?? [], [timeline.data]);
  const [hover, setHover] = useState<number | null>(null);
  const [playing, setPlaying] = useState(false);
  const [verify, setVerify] = useState<Window | null>(null);
  const svgRef = useRef<SVGSVGElement>(null);
  const [width, setWidth] = useState(800);

  useEffect(() => {
    const svg = svgRef.current;
    if (!svg) return;
    const ro = new ResizeObserver(() => setWidth(svg.clientWidth));
    ro.observe(svg);
    return () => ro.disconnect();
  }, []);

  const bucketMs = useMemo(() => {
    if (buckets.length < 2) return 60_000;
    return Date.parse(buckets[1]!.start) - Date.parse(buckets[0]!.start);
  }, [buckets]);

  // The bucket the view sits in (the last one when live).
  const current = useMemo(() => {
    if (!buckets.length) return -1;
    if (view.mode === "live") return buckets.length - 1;
    // Empty buckets share their offset with the one before, so locate by time.
    if (view.time) {
      const i = Math.round((Date.parse(view.time) - Date.parse(buckets[0]!.start)) / bucketMs) - 1;
      return Math.max(0, Math.min(buckets.length - 1, i));
    }
    const i = buckets.findIndex((b) => b.offset >= view.offset);
    return i === -1 ? buckets.length - 1 : i;
  }, [buckets, bucketMs, view]);

  const jump = useCallback(
    (i: number) => {
      const b = buckets[i];
      if (!b) return;
      if (i === buckets.length - 1) goLive();
      else goTo(b.offset, new Date(Date.parse(b.start) + bucketMs).toISOString());
    },
    [buckets, bucketMs, goLive, goTo],
  );

  // Playback: step forward through history, then rejoin live.
  useEffect(() => {
    if (!playing) return;
    let at = current;
    const timer = window.setInterval(() => {
      // Skip stretches when the pipeline wasn't running.
      let next = at + 1;
      while (next < buckets.length - 1 && buckets[next]!.articles === 0) next++;
      if (next >= buckets.length - 1) {
        setPlaying(false);
        goLive();
      } else {
        at = next;
        jump(next);
      }
    }, PLAY_STEP_MS);
    return () => window.clearInterval(timer);
    // `current` is read once when playback starts; the timer advances from there.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [playing, buckets, jump, goLive]);

  const max = Math.max(1, ...buckets.map((b) => b.articles));
  // Stretches with no input at all: the pipeline was off, not the news.
  const idle = useMemo(() => {
    const runs: { from: number; to: number }[] = [];
    let start = -1;
    buckets.forEach((b, i) => {
      if (b.articles === 0 && start < 0) start = i;
      if ((b.articles > 0 || i === buckets.length - 1) && start >= 0) {
        const end = b.articles > 0 ? i : i + 1;
        if (end - start >= 3) runs.push({ from: start, to: end });
        start = -1;
      }
    });
    return runs;
  }, [buckets]);
  const barW = buckets.length ? width / buckets.length : 0;
  const indexAt = (clientX: number) => {
    const rect = svgRef.current!.getBoundingClientRect();
    return Math.max(0, Math.min(buckets.length - 1, Math.floor(((clientX - rect.left) / rect.width) * buckets.length)));
  };

  // "Re-run this hour": the hour of inputs ending at the view's bucket.
  const hourWindow = (): Window | null => {
    const end = buckets[current];
    if (!end) return null;
    const endTime = Date.parse(end.start) + bucketMs;
    const firstIdx = buckets.findIndex((b) => Date.parse(b.start) >= endTime - 3_600_000);
    const before = buckets[firstIdx - 1];
    const from = before ? before.offset + 1 : 0;
    return { from, to: end.offset + 1, label: `${clockTime(Math.max(Date.parse(buckets[0]!.start), endTime - 3_600_000))} – ${clockTime(endTime)}` };
  };

  const hovered = hover !== null ? buckets[hover] : null;

  return (
    <footer className="timeline" aria-label="Time travel">
      <div className="timeline__controls">
        <button
          className={`btn btn--live ${view.mode === "live" ? "is-active" : ""}`}
          onClick={() => {
            setPlaying(false);
            goLive();
          }}
          aria-pressed={view.mode === "live"}
        >
          <Radio size={14} aria-hidden /> Live
        </button>
        <button
          className="icon-btn"
          onClick={() => {
            if (!playing && view.mode === "live") jump(Math.max(0, buckets.length - 40));
            setPlaying((p) => !p);
          }}
          aria-label={playing ? "Pause playback" : "Play history"}
          title={playing ? "Pause" : "Play history"}
        >
          {playing ? <Pause size={16} /> : <Play size={16} />}
        </button>
      </div>

      <div className="timeline__chart">
        <svg
          ref={svgRef}
          className="timeline__svg"
          height={HEIGHT}
          role="slider"
          tabIndex={0}
          aria-label="Time travel: choose a moment to view"
          aria-valuemin={0}
          aria-valuemax={Math.max(0, buckets.length - 1)}
          aria-valuenow={Math.max(0, current)}
          aria-valuetext={view.mode === "live" ? "Live" : view.time ? clockTime(view.time) : undefined}
          onPointerDown={(e) => {
            if (!buckets.length) return;
            setPlaying(false);
            (e.target as Element).setPointerCapture?.(e.pointerId);
            jump(indexAt(e.clientX));
          }}
          onPointerMove={(e) => {
            if (!buckets.length) return;
            const i = indexAt(e.clientX);
            setHover(i);
            if (e.buttons === 1) jump(i);
          }}
          onPointerLeave={() => setHover(null)}
          onKeyDown={(e) => {
            const step = e.shiftKey ? 10 : 1;
            if (e.key === "ArrowLeft") jump(Math.max(0, current - step));
            else if (e.key === "ArrowRight") jump(Math.min(buckets.length - 1, current + step));
            else if (e.key === "End") goLive();
            else if (e.key === "Home") jump(0);
            else return;
            e.preventDefault();
          }}
        >
          <defs>
            <pattern id="idle-hatch" width="6" height="6" patternUnits="userSpaceOnUse" patternTransform="rotate(45)">
              <line x1="0" y1="0" x2="0" y2="6" className="timeline__hatch" />
            </pattern>
          </defs>
          {idle.map((r) => {
            const x = r.from * barW;
            const w = (r.to - r.from) * barW;
            return (
              <g key={r.from} className="timeline__idle">
                <rect x={x} y={14} width={w} height={HEIGHT - 14} fill="url(#idle-hatch)" />
                {w > 110 && (
                  <text x={x + w / 2} y={HEIGHT / 2 + 8} textAnchor="middle">
                    pipeline off · {duration(r.to - r.from, bucketMs)}
                  </text>
                )}
              </g>
            );
          })}
          {buckets.map((b, i) => {
            // Square-root scale so quiet intervals stay visible next to a backfill burst.
            const h = Math.max(b.articles ? 2 : 0, Math.sqrt(b.articles / max) * (HEIGHT - 16));
            const past = i <= current;
            return (
              <g key={i}>
                <rect
                  x={i * barW + 0.5}
                  y={HEIGHT - h}
                  width={Math.max(1, barW - 1.5)}
                  height={h}
                  rx={Math.min(2, barW / 3)}
                  className={`timeline__bar ${past ? "is-past" : ""} ${i === hover ? "is-hover" : ""}`}
                />
                {b.merges + b.splits > 0 && (
                  <circle cx={i * barW + barW / 2} cy={5} r={2.75} className="timeline__lineage" />
                )}
              </g>
            );
          })}
          {current >= 0 && (
            <line
              x1={(current + 1) * barW}
              x2={(current + 1) * barW}
              y1={0}
              y2={HEIGHT}
              className={`timeline__playhead ${view.mode === "live" ? "is-live" : ""}`}
            />
          )}
        </svg>
        <div className="timeline__axis" aria-hidden>
          <span>{buckets[0] ? clockTime(buckets[0].start) : ""}</span>
          <span>{hovered ? <TimelineHover bucket={hovered} /> : <Legend />}</span>
          <span>{view.mode === "live" ? "now" : view.time ? clockTime(view.time) : ""}</span>
        </div>
      </div>

      <div className="timeline__verify">
        <button
          className="btn"
          onClick={() => setVerify(hourWindow())}
          disabled={!buckets.length}
          title="Re-run the pipeline over this hour of input and compare with what it produced live"
        >
          <ShieldCheck size={15} aria-hidden /> Re-run this hour
        </button>
      </div>
      {verify && <ReplayDialog window={verify} onClose={() => setVerify(null)} />}
    </footer>
  );
}

function TimelineHover({ bucket }: { bucket: TimelineBucket }) {
  if (bucket.articles === 0) {
    return (
      <span className="timeline__hover">
        <strong className="num">{clockTime(bucket.start)}</strong> · no input (pipeline idle)
      </span>
    );
  }
  return (
    <span className="timeline__hover">
      <strong className="num">{clockTime(bucket.start)}</strong> · {plural(bucket.articles, "article")} ·{" "}
      {plural(bucket.created, "new story", "new stories")}
      {bucket.merges + bucket.splits > 0 && (
        <span className="ev-lineage-text"> · {count(bucket.merges)} merges, {count(bucket.splits)} splits</span>
      )}
    </span>
  );
}

function Legend() {
  return (
    <span className="timeline__legend">
      <i className="timeline__legend-bar" /> articles per interval <i className="timeline__legend-dot" /> split / merge
    </span>
  );
}

function duration(buckets: number, bucketMs: number): string {
  const minutes = Math.round((buckets * bucketMs) / 60_000);
  if (minutes < 90) return `${minutes} min`;
  return `${Math.round(minutes / 60)} h`;
}
