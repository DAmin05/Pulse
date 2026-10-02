import { ArrowUpRight, Copy, GitFork, GitMerge, Clock, Newspaper, Sparkle, X, CircleOff } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";

import { useStory } from "../api/hooks";
import type { ArticleView, LiveEvent, StoryRef } from "../api/types";
import { clockTime, count, languageName, plural, relativeTime, sourceName } from "../lib/format";
import { useReferenceTime } from "../lib/time";
import { useApp } from "../live/store";
import { ListenPanel } from "./ListenPanel";
import { Sparkline } from "./Sparkline";

const ARTICLES_SHOWN = 25;

export function StoryDrawer() {
  const selected = useApp((s) => s.selected);
  const select = useApp((s) => s.select);
  const story = useStory(selected);
  const live = useApp((s) => s.view.mode === "live");
  const headingRef = useRef<HTMLHeadingElement>(null);
  // "Show all" applies to the story it was clicked on only.
  const [showAllFor, setShowAllFor] = useState<string | null>(null);
  const showAll = showAllFor !== null && showAllFor === selected;
  const setShowAll = () => setShowAllFor(selected);

  useEffect(() => {
    if (selected) headingRef.current?.focus({ preventScroll: true });
  }, [selected]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape" && useApp.getState().selected && !useApp.getState().searchOpen) select(null);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [select]);

  const data = story.data?.story;
  const reference = useReferenceTime();
  const langCounts = useMemo(() => {
    const m = new Map<string, number>();
    for (const a of data?.articles ?? []) m.set(a.lang, (m.get(a.lang) ?? 0) + 1);
    return [...m.entries()].sort((a, b) => b[1] - a[1]);
  }, [data]);

  if (!selected) return null;

  return (
    <aside className="drawer" aria-labelledby="drawer-title">
      <div className="drawer__bar">
        <span className="drawer__kicker">
          {data?.closed_at ? <ClosedBadge reason={data.close_reason} /> : <span className="state state--open">Open story</span>}
        </span>
        <button className="icon-btn" onClick={() => select(null)} aria-label="Close story">
          <X size={18} />
        </button>
      </div>

      {story.isError && <p className="drawer__error">This story doesn't exist at the selected time.</p>}
      {story.isLoading && <DrawerSkeleton />}

      {data && (
        <div className="drawer__scroll">
          {data.lineage.merged_into && (
            <LineageLink
              icon={GitMerge}
              label="Merged into"
              story={data.lineage.merged_into}
              onSelect={select}
              emphasis
            />
          )}
          <h2 id="drawer-title" className="drawer__headline" tabIndex={-1} ref={headingRef}>
            {data.headline}
          </h2>
          <p className="drawer__meta">
            <span className="num">{plural(data.source_count, "source")}</span>
            <span aria-hidden>·</span>
            <span className="num">{plural(data.article_count, "article")}</span>
            <span aria-hidden>·</span>
            <span>first seen {relativeTime(data.created_at, reference)}</span>
          </p>

          {live && <ListenPanel key={data.id} storyId={data.id} headline={data.headline} storyLangs={data.langs} />}

          <section className="drawer__section">
            <h3 className="section-title">Coverage, last 24 hours</h3>
            <Sparkline values={data.activity} width={392} height={56} label="Articles per hour of publication" className="drawer__spark" />
            <div className="axis-labels" aria-hidden>
              <span>24h ago</span>
              <span>now</span>
            </div>
          </section>

          <section className="drawer__section">
            <h3 className="section-title">Languages</h3>
            <ul className="lang-bars">
              {langCounts.map(([lang, n]) => (
                <li key={lang}>
                  <span className="lang-bars__name">{languageName(lang)}</span>
                  <span className="lang-bars__bar" aria-hidden>
                    <i style={{ width: `${(n / langCounts[0]![1]) * 100}%` }} />
                  </span>
                  <span className="lang-bars__n num">{n}</span>
                </li>
              ))}
            </ul>
          </section>

          {(data.lineage.parents.length > 0 || data.lineage.children.length > 0 || data.lineage.merged_from.length > 0) && (
            <section className="drawer__section">
              <h3 className="section-title">Lineage</h3>
              <div className="lineage">
                {data.lineage.parents.map((p) => (
                  <LineageLink key={p.id} icon={GitFork} label="Split from" story={p} onSelect={select} />
                ))}
                {data.lineage.children.map((c) => (
                  <LineageLink key={c.id} icon={GitFork} label="Split into" story={c} onSelect={select} />
                ))}
                {data.lineage.merged_from.map((m) => (
                  <LineageLink key={m.id} icon={GitMerge} label="Absorbed" story={m} onSelect={select} />
                ))}
              </div>
            </section>
          )}

          <section className="drawer__section">
            <h3 className="section-title">
              Articles <span className="num muted">{count(data.articles.length)}</span>
            </h3>
            <ul className="articles">
              {(showAll ? data.articles : data.articles.slice(0, ARTICLES_SHOWN)).map((a) => (
                <ArticleRow key={a.id} article={a} reference={reference} />
              ))}
            </ul>
            {!showAll && data.articles.length > ARTICLES_SHOWN && (
              <button className="text-btn" onClick={setShowAll}>
                Show all {count(data.articles.length)} articles
              </button>
            )}
          </section>

          <section className="drawer__section">
            <h3 className="section-title">History</h3>
            <ol className="history">
              {data.events.slice(0, 40).map((e) => (
                <HistoryRow key={e.event_id} event={e} />
              ))}
            </ol>
          </section>
        </div>
      )}
    </aside>
  );
}

function ClosedBadge({ reason }: { reason: string | null }) {
  const text = reason === "merged" ? "Merged" : reason === "split" ? "Split" : "Closed: quiet for 48h";
  return (
    <span className="state state--closed">
      <CircleOff size={12} aria-hidden />
      {text}
    </span>
  );
}

function LineageLink({
  icon: Icon,
  label,
  story,
  onSelect,
  emphasis,
}: {
  icon: typeof GitFork;
  label: string;
  story: StoryRef;
  onSelect: (id: string) => void;
  emphasis?: boolean;
}) {
  return (
    <button className={`lineage-link ${emphasis ? "lineage-link--emphasis" : ""}`} onClick={() => onSelect(story.id)}>
      <Icon size={14} className="ev-lineage" aria-hidden />
      <span className="lineage-link__label">{label}</span>
      <span className="lineage-link__headline">{story.headline}</span>
    </button>
  );
}

function ArticleRow({ article, reference }: { article: ArticleView; reference: number }) {
  return (
    <li className="article">
      <div className="article__meta">
        <span className="article__source">{sourceName(article.source_id)}</span>
        <span className="lang">{article.lang}</span>
        <span className="muted">{relativeTime(article.published_at, reference)}</span>
        {article.is_duplicate && (
          <span className="tag" title="Near-duplicate of another article (syndicated copy)">
            <Copy size={11} aria-hidden /> copy
          </span>
        )}
        {article.late && (
          <span className="tag" title="Arrived after the watermark (more than 24h late)">
            <Clock size={11} aria-hidden /> late
          </span>
        )}
      </div>
      <a className="article__title" href={article.url} target="_blank" rel="noopener noreferrer">
        {article.title}
        <ArrowUpRight size={13} aria-hidden className="article__ext" />
        <span className="sr-only"> (opens the publisher's site)</span>
      </a>
    </li>
  );
}

function HistoryRow({ event }: { event: LiveEvent }) {
  const [Icon, tone, text] = (() => {
    switch (event.kind) {
      case "created":
        return [Sparkle, "birth", event.parent_ids.length ? "Born from a split" : "Story began"] as const;
      case "article_added":
        return [Newspaper, "article", event.is_duplicate ? "Syndicated copy added" : event.late ? "Late article joined" : "Article joined"] as const;
      case "updated":
        return [Newspaper, "article", `${plural(event.source_count, "source")}, ${event.langs.length} languages`] as const;
      case "merged":
        return [GitMerge, "lineage", `Absorbed ${plural(event.source_ids.length, "story", "stories")}`] as const;
      case "split":
        return [GitFork, "lineage", `Split into ${event.children.length}`] as const;
      case "closed":
        return [CircleOff, "lineage", event.reason === "idle" ? "Closed after 48h quiet" : `Closed (${event.reason})`] as const;
    }
  })();
  return (
    <li className="history__row">
      <Icon size={13} className={`ev-${tone}`} aria-hidden />
      <span>{text}</span>
      <span className="muted num">{clockTime(event.event_time)}</span>
    </li>
  );
}

function DrawerSkeleton() {
  return (
    <div className="drawer__scroll" aria-hidden>
      <div className="skeleton skeleton--title" />
      <div className="skeleton skeleton--line" />
      <div className="skeleton skeleton--block" />
      <div className="skeleton skeleton--line" />
      <div className="skeleton skeleton--line" />
    </div>
  );
}
