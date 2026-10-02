import { ArrowUpRight, AudioLines, Languages, LoaderCircle, Pause, Play, RotateCcw } from "lucide-react";
import { memo, useState } from "react";

import { ApiError } from "../api/client";
import { useBriefing, useListen } from "../api/hooks";
import type { Briefing, ListenCapabilities } from "../api/types";
import { count, languageName } from "../lib/format";
import { clock, layout, type SegmentLayout } from "../listen/timing";
import { speechSupported, useNarration } from "../listen/useNarration";

const LANG_KEY = "pulse.listen.lang";
const VOICE_KEY = "pulse.listen.voice";

function stored(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

function store(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    // Private mode: the choice just isn't remembered.
  }
}

/** The remembered language, else the browser's, else English. */
function initialLang(caps: ListenCapabilities | undefined): string {
  const supported = (code: string | null | undefined) =>
    code && caps?.languages.some((l) => l.code === code) ? code : null;
  return supported(stored(LANG_KEY)) ?? supported(navigator.language.split("-")[0]) ?? "en";
}

/** "Listen in any language": the story briefing, translated and read aloud. */
export function ListenPanel({ storyId, headline, storyLangs }: { storyId: string; headline: string; storyLangs: string[] }) {
  const caps = useListen();
  const [chosenLang, setChosenLang] = useState<string | null>(null);
  const [chosenVoice, setChosenVoice] = useState<string | null>(stored(VOICE_KEY));
  const [requested, setRequested] = useState(false);
  // When play was pressed: a briefing fetched before then is a free replay.
  const [requestedAt, setRequestedAt] = useState(0);
  const lang = chosenLang ?? initialLang(caps.data);
  const voices = caps.data?.speech?.voices ?? [];
  const voice = voices.some((v) => v.id === chosenVoice) ? chosenVoice! : undefined;
  const briefing = useBriefing(storyId, lang, voice, requested);

  // Without speech anywhere (old API, no browser voices) there's nothing to offer.
  if (caps.isError || (caps.data && !caps.data.speech && !speechSupported)) return null;

  const languages = caps.data?.languages ?? [];
  const choose = (next: { lang?: string; voice?: string }) => {
    if (next.lang) {
      setChosenLang(next.lang);
      store(LANG_KEY, next.lang);
    }
    if (next.voice) {
      setChosenVoice(next.voice);
      store(VOICE_KEY, next.voice);
    }
    setRequested(false);
  };
  const unavailable = briefing.error instanceof ApiError && briefing.error.status === 422;
  const alternatives = languages.filter((l) => storyLangs.includes(l.code) && l.code !== lang).slice(0, 3);

  return (
    <section className="listen" aria-label="Listen to this story">
      <div className="listen__controls">
        <label className="select select--compact" title="Language">
          <Languages size={14} aria-hidden />
          <span className="sr-only">Language</span>
          <select value={lang} onChange={(e) => choose({ lang: e.target.value })} disabled={!languages.length}>
            {languages.map((l) => (
              <option key={l.code} value={l.code}>
                {l.native}
              </option>
            ))}
          </select>
        </label>
        {voices.length > 1 && (
          <label className="select select--compact" title="Voice">
            <AudioLines size={14} aria-hidden />
            <span className="sr-only">Voice</span>
            <select value={voice ?? caps.data?.speech?.default_voice} onChange={(e) => choose({ voice: e.target.value })}>
              {voices.map((v) => (
                <option key={v.id} value={v.id}>
                  {v.name}
                  {v.description ? ` · ${v.description}` : ""}
                </option>
              ))}
            </select>
          </label>
        )}
      </div>

      {/* Cached data survives a language switch; play only once asked to. */}
      {requested && briefing.data ? (
        <Player
          key={`${storyId}:${lang}:${voice ?? ""}`}
          briefing={briefing.data}
          replay={briefing.dataUpdatedAt < requestedAt}
          headline={headline}
          caps={caps.data}
        />
      ) : (
        <div className="listen__bar">
          <button
            className="listen__play"
            onClick={() => {
              setRequested(true);
              setRequestedAt(Date.now());
            }}
            disabled={briefing.isFetching || !caps.data}
            aria-label={briefing.isFetching ? "Preparing the briefing" : "Listen to a briefing"}
          >
            {briefing.isFetching ? <LoaderCircle size={20} className="spin" aria-hidden /> : <Play size={20} aria-hidden />}
          </button>
          <div className="listen__lead">
            <span className="listen__title">
              {briefing.isFetching ? preparing(caps.data, lang, storyLangs) : "Listen to a briefing"}
            </span>
            <span className="listen__sub">
              {briefing.isFetching
                ? "Usually a few seconds; replays are instant."
                : `About 30 seconds, read in ${languageName(lang)}${caps.data?.speech ? "" : " by your browser"}.`}
            </span>
          </div>
        </div>
      )}

      {briefing.isError && (
        <div className="listen__problem" role="alert">
          <p>{unavailable ? briefing.error.message : `Couldn't prepare the briefing: ${briefing.error.message}`}</p>
          {unavailable && alternatives.length > 0 && (
            <p className="listen__alts">
              {alternatives.map((l) => (
                <button key={l.code} className="chip" onClick={() => choose({ lang: l.code })}>
                  Listen in {l.name}
                </button>
              ))}
            </p>
          )}
        </div>
      )}
    </section>
  );
}

function preparing(caps: ListenCapabilities | undefined, lang: string, storyLangs: string[]): string {
  const translating = caps?.translation && storyLangs.some((l) => l !== lang);
  if (translating && caps?.speech) return "Translating and recording…";
  if (caps?.speech) return "Recording…";
  return translating ? "Translating…" : "Preparing…";
}

function Player({
  briefing,
  replay,
  headline,
  caps,
}: {
  briefing: Briefing;
  /** Already fetched in this session: nothing was spent this time. */
  replay: boolean;
  headline: string;
  caps: ListenCapabilities | undefined;
}) {
  const n = useNarration(briefing, true, headline);
  const [items] = useState(() => layout(briefing.segments));
  const playing = n.phase === "playing";
  const busy = n.phase === "loading";
  const fraction = n.duration > 0 ? Math.min(1, n.position / n.duration) : 0;
  const translatedFrom = [...new Set(briefing.segments.filter((s) => s.translated).map((s) => s.original_lang))];

  return (
    <>
      <div className="listen__bar">
        <button
          className={`listen__play ${playing ? "is-playing" : ""}`}
          onClick={playing ? n.pause : n.play}
          disabled={busy || n.engine === null}
          aria-label={playing ? "Pause" : n.phase === "ended" ? "Play again" : "Play"}
        >
          {busy ? (
            <LoaderCircle size={20} className="spin" aria-hidden />
          ) : playing ? (
            <Pause size={20} aria-hidden />
          ) : n.phase === "ended" ? (
            <RotateCcw size={18} aria-hidden />
          ) : (
            <Play size={20} aria-hidden />
          )}
        </button>
        <div className="listen__lead">
          <input
            className="listen__progress"
            type="range"
            min={0}
            max={1000}
            step={1}
            value={Math.round(fraction * 1000)}
            onChange={(e) => n.seekToFraction(Number(e.target.value) / 1000)}
            aria-label="Position in the briefing"
            aria-valuetext={n.engine === "audio" ? `${clock(n.position)} of ${clock(n.duration)}` : `${Math.round(fraction * 100)}%`}
            style={{ "--fill": `${fraction * 100}%` } as React.CSSProperties}
            disabled={busy}
          />
          <span className="listen__times num">
            {n.engine === "audio" ? (
              <>
                <span>{clock(n.position)}</span>
                <span>{clock(n.duration)}</span>
              </>
            ) : (
              <>
                <span>{n.phase === "playing" ? "Reading…" : n.phase === "ended" ? "Done" : "Ready"}</span>
                <span>{Math.round(fraction * 100)}%</span>
              </>
            )}
          </span>
        </div>
      </div>

      <p className="listen__meta">
        {n.engine === "audio" && briefing.audio ? (
          <>
            <span className="listen__badge">
              <AudioLines size={12} aria-hidden /> {briefing.audio.voice_name ?? "ElevenLabs"} ·{" "}
              <a href="https://elevenlabs.io/text-to-speech" target="_blank" rel="noopener noreferrer">
                ElevenLabs
              </a>
            </span>
            <span>
              {briefing.audio.cached || replay
                ? "Replayed from cache: no credits used"
                : `${count(briefing.audio.characters)} characters${budgetNote(caps)}`}
            </span>
          </>
        ) : (
          <>
            <span className="listen__badge listen__badge--browser">Your browser's voice</span>
            {briefing.fallback && briefing.fallback.reason !== "not_configured" && <span>{briefing.fallback.message}</span>}
          </>
        )}
        {briefing.translation && translatedFrom.length > 0 && (
          <span>
            Translated from {translatedFrom.map(languageName).join(", ")} by{" "}
            {briefing.translation.provider === "deepl" ? "DeepL" : "LibreTranslate"}
          </span>
        )}
      </p>
      {briefing.notice && <p className="listen__notice">{briefing.notice}</p>}
      {n.error && (
        <p className="listen__notice" role="alert">
          {n.error}
        </p>
      )}

      <Transcript items={items} active={n.activeOffset} onSeek={n.seekToOffset} lang={briefing.bcp47} />
    </>
  );
}

function budgetNote(caps: ListenCapabilities | undefined): string {
  const b = caps?.speech?.budget;
  if (!b) return "";
  return ` · ${count(Math.max(0, b.daily_limit - b.used_today))} left today`;
}

/** The script, with the word being read highlighted; click a word to jump there. */
const Transcript = memo(function Transcript({
  items,
  active,
  onSeek,
  lang,
}: {
  items: SegmentLayout[];
  active: number | null;
  onSeek: (offset: number) => void;
  lang: string;
}) {
  return (
    <ol className="transcript" lang={lang} aria-label="Transcript">
      {items.map((it) => (
        <li key={it.start} className={`transcript__seg transcript__seg--${it.segment.kind}`}>
          <p>
            {it.tokens.map((t, i) =>
              t.space ? (
                t.text
              ) : (
                <span
                  key={i}
                  className={
                    active === null ? undefined : t.offset === active ? "is-active" : t.offset < active ? "is-spoken" : undefined
                  }
                  onClick={() => onSeek(t.offset)}
                >
                  {t.text}
                </span>
              ),
            )}
          </p>
          {it.segment.source && (
            <a className="transcript__source" href={it.segment.source.url} target="_blank" rel="noopener noreferrer">
              {it.segment.source.name}
              {it.segment.translated && ` · translated from ${languageName(it.segment.original_lang)}`}
              <ArrowUpRight size={12} aria-hidden />
              <span className="sr-only"> (opens the article)</span>
            </a>
          )}
        </li>
      ))}
    </ol>
  );
});
