import { Activity, History, Monitor, Moon, PanelLeft, Search, Sun } from "lucide-react";

import { useStats } from "../api/hooks";
import { clockTime, count, languageName } from "../lib/format";
import { useApp, type Theme } from "../live/store";

const THEMES: { value: Theme; label: string; icon: typeof Sun }[] = [
  { value: "light", label: "Light theme", icon: Sun },
  { value: "dark", label: "Dark theme", icon: Moon },
  { value: "system", label: "Match system theme", icon: Monitor },
];

const LANGS = ["en", "es", "fr", "de", "pt", "ar", "ru", "zh", "ko", "ja", "it", "tr", "uk", "fa", "hi"];

export function TopBar() {
  const view = useApp((s) => s.view);
  const connection = useApp((s) => s.connection);
  const missed = useApp((s) => s.missed);
  const lang = useApp((s) => s.lang);
  const theme = useApp((s) => s.theme);
  const metricsOpen = useApp((s) => s.metricsOpen);
  const { goLive, setLang, setTheme, toggleMetrics, setSearchOpen, toggleFeed } = useApp.getState();
  const stats = useStats();
  const totals = stats.data?.totals;
  const nextTheme = THEMES[(THEMES.findIndex((t) => t.value === theme) + 1) % THEMES.length]!;
  const ThemeIcon = THEMES.find((t) => t.value === theme)!.icon;

  return (
    <header className="topbar">
      <button className="icon-btn topbar__feed-toggle" onClick={() => toggleFeed()} aria-label="Show stories">
        <PanelLeft size={18} />
      </button>
      <div className="brand">
        <svg viewBox="0 0 32 32" width="26" height="26" aria-hidden>
          <path
            d="M3 17h6l3.5-8 5.5 15 3.5-7H29"
            fill="none"
            stroke="var(--accent)"
            strokeWidth="2.6"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
        <span className="brand__name">Pulse</span>
      </div>

      {view.mode === "live" ? (
        <div className={`status status--${connection}`} role="status" aria-live="polite">
          <span className="status__dot" aria-hidden />
          {connection === "live" ? "Live" : connection === "connecting" ? "Connecting…" : "Reconnecting…"}
        </div>
      ) : (
        <div className="status status--past" role="status">
          <History size={14} aria-hidden />
          <span>Viewing {view.time ? clockTime(view.time) : `offset ${view.offset}`}</span>
          <button className="status__live" onClick={goLive}>
            Back to live{missed > 0 && <span className="badge num">{count(missed)} new</span>}
          </button>
        </div>
      )}

      {totals && (
        <p className="topbar__totals">
          <span className="num">{count(totals.stories_open)}</span> stories ·{" "}
          <span className="num">{count(totals.articles)}</span> articles ·{" "}
          <span className="num">{totals.languages}</span> languages
        </p>
      )}

      <div className="topbar__spacer" />

      <button className="search-trigger" onClick={() => setSearchOpen(true)}>
        <Search size={15} aria-hidden />
        <span>Search any language</span>
        <kbd>⌘K</kbd>
      </button>

      <label className="select">
        <span className="sr-only">Language</span>
        <select value={lang ?? ""} onChange={(e) => setLang(e.target.value || null)}>
          <option value="">All languages</option>
          {LANGS.map((l) => (
            <option key={l} value={l}>
              {languageName(l)}
            </option>
          ))}
        </select>
      </label>

      <button
        className={`icon-btn ${metricsOpen ? "is-active" : ""}`}
        onClick={() => toggleMetrics()}
        aria-pressed={metricsOpen}
        aria-label="Pipeline metrics"
        title="Pipeline metrics"
      >
        <Activity size={18} />
      </button>
      <button
        className="icon-btn"
        onClick={() => setTheme(nextTheme.value)}
        aria-label={`${THEMES.find((t) => t.value === theme)!.label}. Switch to ${nextTheme.label.toLowerCase()}`}
        title={nextTheme.label}
      >
        <ThemeIcon size={18} />
      </button>
    </header>
  );
}
