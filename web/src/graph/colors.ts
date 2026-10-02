// Canvas can't use CSS variables, so the renderer reads resolved token values
// and re-reads them when the theme changes.

export interface Palette {
  bg: string;
  ink1: string;
  ink2: string;
  ink3: string;
  grid: string;
  surface1: string;
  birth: string;
  article: string;
  lineage: string;
  /** Recency ramp: [hot, warm, cool, cold]. */
  recency: [string, string, string, string];
}

export function readPalette(): Palette {
  const css = getComputedStyle(document.documentElement);
  const v = (name: string) => css.getPropertyValue(name).trim();
  return {
    bg: v("--graph-bg"),
    ink1: v("--ink-1"),
    ink2: v("--ink-2"),
    ink3: v("--ink-3"),
    grid: v("--grid"),
    surface1: v("--surface-1"),
    birth: v("--ev-birth"),
    article: v("--ev-article"),
    lineage: v("--ev-lineage"),
    recency: [v("--seq-hot"), v("--seq-warm"), v("--seq-cool"), v("--seq-cold")],
  };
}

function rgb(hex: string): [number, number, number] {
  const h = hex.replace("#", "");
  const n = parseInt(h.length === 3 ? h.replace(/./g, (c) => c + c) : h, 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

function mix(a: string, b: string, t: number): string {
  const [ar, ag, ab] = rgb(a);
  const [br, bg, bb] = rgb(b);
  const c = (x: number, y: number) => Math.round(x + (y - x) * t);
  return `rgb(${c(ar, br)},${c(ag, bg)},${c(ab, bb)})`;
}

/** Hours since the story's latest article → a step on the recency ramp. */
const STOPS = [0, 1, 6, 24];

export function recencyColor(palette: Palette, hoursAgo: number): string {
  const h = Math.max(0, hoursAgo);
  for (let i = 0; i < STOPS.length - 1; i++) {
    const lo = STOPS[i]!;
    const hi = STOPS[i + 1]!;
    if (h <= hi) return mix(palette.recency[i]!, palette.recency[i + 1]!, (h - lo) / (hi - lo));
  }
  return palette.recency[3];
}

/** Same color with alpha, for rings and fades. */
export function withAlpha(color: string, alpha: number): string {
  if (color.startsWith("rgb(")) return color.replace("rgb(", "rgba(").replace(")", `,${alpha})`);
  const [r, g, b] = rgb(color);
  return `rgba(${r},${g},${b},${alpha})`;
}
