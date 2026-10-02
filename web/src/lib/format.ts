const languageNames = new Intl.DisplayNames(["en"], { type: "language" });

/** "es" → "Spanish". */
export function languageName(code: string): string {
  if (!code || code === "und") return "Unknown";
  try {
    return languageNames.of(code) ?? code;
  } catch {
    return code;
  }
}

const ACRONYMS = new Set([
  "bbc", "nyt", "dw", "npr", "cnbc", "abc", "cbs", "ndtv", "scmp", "un", "toi", "rfi",
  "ansa", "nos", "g1", "orf", "nzz", "faz", "us", "uk", "hn", "wsj", "en", "es", "fr",
  "de", "pt", "ar", "ru", "zh", "ko", "hi", "ur", "fa", "tr", "id", "vi", "th", "sw", "pl", "el",
]);

/** "bbc-world" → "BBC World"; "gdelt-translingual:lemonde.fr" → "lemonde.fr". */
export function sourceName(id: string): string {
  const colon = id.indexOf(":");
  if (colon >= 0) return id.slice(colon + 1);
  return id
    .split("-")
    .map((w) => (ACRONYMS.has(w) ? w.toUpperCase() : w.charAt(0).toUpperCase() + w.slice(1)))
    .join(" ");
}

/** "3m ago", "2h ago", "Oct 1" relative to `reference` (ms). */
export function relativeTime(at: string | number, reference: number = Date.now()): string {
  const t = typeof at === "number" ? at : Date.parse(at);
  const seconds = Math.round((reference - t) / 1000);
  if (seconds < 45) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 36) return `${hours}h ago`;
  return new Date(t).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/** "14:32" or "Oct 1, 14:32" when not today. */
export function clockTime(at: string | number): string {
  const d = new Date(at);
  const today = new Date().toDateString() === d.toDateString();
  return d.toLocaleString(undefined, {
    ...(today ? {} : { month: "short", day: "numeric" }),
    hour: "2-digit",
    minute: "2-digit",
  });
}

const compact = new Intl.NumberFormat("en", { notation: "compact", maximumFractionDigits: 1 });
const plain = new Intl.NumberFormat("en");

export function count(n: number): string {
  return n >= 10_000 ? compact.format(n) : plain.format(n);
}

/** Metric values with sensible precision for their size. */
export function metric(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  if (Math.abs(v) >= 100) return plain.format(Math.round(v));
  if (Math.abs(v) >= 10) return v.toFixed(1);
  return v.toFixed(2);
}

export function plural(n: number, one: string, many = `${one}s`): string {
  return `${count(n)} ${n === 1 ? one : many}`;
}
