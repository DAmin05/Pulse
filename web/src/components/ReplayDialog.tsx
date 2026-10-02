import { useQuery } from "@tanstack/react-query";
import { CircleCheck, CircleX, LoaderCircle, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { api } from "../api/client";
import type { Replay } from "../api/types";
import { count } from "../lib/format";

interface Props {
  window: { from: number; to: number; label: string };
  onClose: () => void;
}

/**
 * Starts a replay of the window and shows the determinism proof: the replayed
 * output compared byte for byte with what the live pipeline committed.
 */
export function ReplayDialog({ window: win, onClose }: Props) {
  const dialogRef = useRef<HTMLDialogElement>(null);
  const [id, setId] = useState<string | null>(null);
  const [startError, setStartError] = useState<string | null>(null);

  useEffect(() => {
    dialogRef.current?.showModal();
    let cancelled = false;
    api
      .startReplay({ from: win.from, to: win.to })
      .then((r) => !cancelled && setId(r.id))
      .catch((e: Error) => !cancelled && setStartError(e.message));
    return () => {
      cancelled = true;
    };
  }, [win.from, win.to]);

  const replay = useQuery({
    queryKey: ["replay", id],
    queryFn: () => api.replay(id!),
    enabled: id !== null,
    refetchInterval: (q) => {
      const status = (q.state.data as Replay | undefined)?.status;
      return status === "done" || status === "failed" ? false : 600;
    },
  });
  const r = replay.data;
  const report = r?.report;

  return (
    <dialog ref={dialogRef} className="dialog" onClose={onClose} aria-labelledby="replay-title">
      <div className="dialog__head">
        <div>
          <h2 id="replay-title" className="dialog__title">
            Re-run {win.label}
          </h2>
          <p className="dialog__sub">
            Re-processes input offsets <span className="num">{count(win.from)}</span>–
            <span className="num">{count(win.to)}</span> from the Kafka log and compares the result, byte for byte, with what
            the live pipeline produced.
          </p>
        </div>
        <button className="icon-btn" onClick={() => dialogRef.current?.close()} aria-label="Close">
          <X size={18} />
        </button>
      </div>

      {startError && <Verdict ok={false} title="Couldn't start the replay" detail={startError} />}
      {!startError && (!r || r.status === "queued" || r.status === "running") && (
        <div className="verdict verdict--pending" role="status">
          <LoaderCircle size={22} className="spin" aria-hidden />
          <div>
            <p className="verdict__title">{r?.status === "running" ? "Replaying…" : "Queued…"}</p>
            <p className="verdict__detail">Restoring the nearest snapshot, then re-driving the window.</p>
          </div>
        </div>
      )}
      {r?.status === "failed" && <Verdict ok={false} title="Replay failed" detail={r.error ?? "unknown error"} />}
      {report && (
        <>
          <Verdict
            ok={report.identical}
            title={report.identical ? "Identical" : "Different"}
            detail={
              report.identical
                ? `Every story event and late article matched, in order.${report.clamped ? " (Window ends where the live pipeline has committed.)" : ""}`
                : "The replayed output differs from what was committed live."
            }
          />
          <dl className="proof">
            <div>
              <dt>Story events</dt>
              <dd className="num">
                {count(report.events.matched)} / {count(report.events.original)} matched
              </dd>
            </div>
            <div>
              <dt>Late articles</dt>
              <dd className="num">
                {count(report.late.replayed)} / {count(report.late.original)}
              </dd>
            </div>
            <div>
              <dt>Inputs re-processed</dt>
              <dd className="num">{count(report.inputs)}</dd>
            </div>
            <div>
              <dt>Started from</dt>
              <dd>
                {report.snapshot_offset !== null ? `snapshot @${count(report.snapshot_offset)}` : "the log's beginning"} +{" "}
                <span className="num">{count(report.warmup_inputs)}</span> warm-up
              </dd>
            </div>
            <div>
              <dt>Took</dt>
              <dd className="num">{report.seconds.toFixed(2)} s</dd>
            </div>
            <div className="proof__hash">
              <dt>Live output hash</dt>
              <dd className="mono">{report.events.original_hash.slice(0, 32)}…</dd>
            </div>
            <div className="proof__hash">
              <dt>Replay output hash</dt>
              <dd className="mono">{report.events.replayed_hash.slice(0, 32)}…</dd>
            </div>
          </dl>
          {report.events.first_divergence && (
            <div className="divergence">
              <p className="section-title">First difference, event #{report.events.first_divergence.position}</p>
              <pre>live:   {report.events.first_divergence.original ?? "(none)"}</pre>
              <pre>replay: {report.events.first_divergence.replayed ?? "(none)"}</pre>
            </div>
          )}
        </>
      )}
    </dialog>
  );
}

function Verdict({ ok, title, detail }: { ok: boolean; title: string; detail: string }) {
  const Icon = ok ? CircleCheck : CircleX;
  return (
    <div className={`verdict ${ok ? "verdict--ok" : "verdict--bad"}`} role="status">
      <Icon size={26} aria-hidden />
      <div>
        <p className="verdict__title">{title}</p>
        <p className="verdict__detail">{detail}</p>
      </div>
    </div>
  );
}
