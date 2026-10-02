import { useId } from "react";

interface Props {
  values: number[];
  width?: number;
  height?: number;
  /** Accessible summary, e.g. "Articles per hour over the last 24 hours". */
  label: string;
  className?: string;
}

/** Area sparkline: 2px line, soft fill to the baseline, a dot on the latest value. */
export function Sparkline({ values, width = 120, height = 28, label, className }: Props) {
  const gradient = useId();
  if (values.length < 2) {
    return <svg width={width} height={height} className={className} role="img" aria-label={`${label}: no data`} />;
  }
  const max = Math.max(...values, 1);
  const pad = 2;
  const step = (width - pad * 2) / (values.length - 1);
  const y = (v: number) => height - pad - (v / max) * (height - pad * 2);
  const points = values.map((v, i) => [pad + i * step, y(v)] as const);
  const line = points.map(([px, py], i) => `${i ? "L" : "M"}${px.toFixed(1)},${py.toFixed(1)}`).join("");
  const area = `${line}L${points.at(-1)![0].toFixed(1)},${height - pad}L${pad},${height - pad}Z`;
  const [lx, ly] = points.at(-1)!;
  const total = values.reduce((a, b) => a + b, 0);

  return (
    <svg
      width={width}
      height={height}
      viewBox={`0 0 ${width} ${height}`}
      className={className}
      role="img"
      aria-label={`${label}: ${total} total, latest ${values.at(-1)}`}
    >
      <defs>
        <linearGradient id={gradient} x1="0" x2="0" y1="0" y2="1">
          <stop offset="0%" stopColor="var(--spark)" stopOpacity="0.28" />
          <stop offset="100%" stopColor="var(--spark)" stopOpacity="0" />
        </linearGradient>
      </defs>
      <path d={area} fill={`url(#${gradient})`} />
      <path d={line} fill="none" stroke="var(--spark)" strokeWidth="2" strokeLinejoin="round" strokeLinecap="round" />
      <circle cx={lx} cy={ly} r="2.75" fill="var(--spark)" stroke="var(--surface-1)" strokeWidth="1.5" />
    </svg>
  );
}
