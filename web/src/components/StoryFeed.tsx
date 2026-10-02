import { GitFork, Newspaper, Sparkle, X } from "lucide-react";

import { useStories } from "../api/hooks";
import type { StoryCard } from "../api/types";
import { languageName, plural, relativeTime, sourceName } from "../lib/format";
import { useReferenceTime } from "../lib/time";
import { useApp, type TickerItem } from "../live/store";
import { Sparkline } from "./Sparkline";

const EVENT_ICON = { created: Sparkle, article_added: Newspaper, merged: GitFork, split: GitFork } as const;

export function StoryFeed() {
  const stories = useStories(40);
  const selected = useApp((s) => s.selected);
  const select = useApp((s) => s.select);
  const view = useApp((s) => s.view);
  const lang = useApp((s) => s.lang);
  const feedOpen = useApp((s) => s.feedOpen);
  const toggleFeed = useApp((s) => s.toggleFeed);
  const reference = useReferenceTime();

  return (
    <nav className={`rail ${feedOpen ? "is-open" : ""}`} aria-label="Top stories">
      <div className="rail__head">
        <h2 className="rail__title">
          {view.mode === "live" ? "What the world is covering" : "Top stories then"}
        </h2>
        <p className="rail__sub">
          Ranked by distinct sources{lang ? ` · in ${languageName(lang)}` : ""}
        </p>
        <button className="icon-btn rail__close" onClick={() => toggleFeed(false)} aria-label="Close stories">
          <X size={18} />
        </button>
      </div>

      {view.mode === "live" && <Ticker />}

      <ol className="feed">
        {stories.isLoading &&
          Array.from({ length: 6 }, (_, i) => (
            <li key={i} className="card card--skeleton" aria-hidden>
              <span />
              <span />
              <span />
            </li>
          ))}
        {stories.data?.stories.map((story, i) => (
          <li key={story.id}>
            <FeedCard
              story={story}
              rank={i + 1}
              selected={story.id === selected}
              reference={reference}
              onSelect={() => {
                select(story.id);
                toggleFeed(false);
              }}
            />
          </li>
        ))}
        {stories.isError && <li className="feed__empty">Couldn't load stories. Retrying…</li>}
        {stories.data && stories.data.stories.length === 0 && (
          <li className="feed__empty">No multi-source stories{lang ? ` in ${languageName(lang)}` : ""} yet.</li>
        )}
      </ol>
    </nav>
  );
}

function FeedCard({
  story,
  rank,
  selected,
  reference,
  onSelect,
}: {
  story: StoryCard;
  rank: number;
  selected: boolean;
  reference: number;
  onSelect: () => void;
}) {
  const lineage = story.merged_from.length > 0 || story.parent_ids.length > 0;
  return (
    <button className={`card ${selected ? "is-selected" : ""}`} onClick={onSelect} aria-current={selected || undefined}>
      <span className="card__rank num" aria-hidden>
        {rank}
      </span>
      <span className="card__body">
        <span className="card__headline">{story.headline}</span>
        <span className="card__meta">
          <span className="num">{plural(story.source_count, "source")}</span>
          <span aria-hidden>·</span>
          <span className="num">{plural(story.article_count, "article")}</span>
          <span aria-hidden>·</span>
          <span>{relativeTime(story.updated_at, reference)}</span>
        </span>
        <span className="card__foot">
          <span className="langs" title={story.langs.map(languageName).join(", ")}>
            {story.langs.slice(0, 5).map((l) => (
              <span key={l} className="lang">
                {l}
              </span>
            ))}
            {story.langs.length > 5 && <span className="lang lang--more">+{story.langs.length - 5}</span>}
          </span>
          {lineage && (
            <span className="lineage-tag" title="Formed by a merge or split">
              <GitFork size={11} aria-hidden />
              {story.merged_from.length > 0 ? `merged ×${story.merged_from.length}` : "split"}
            </span>
          )}
          <Sparkline values={story.activity} width={84} height={22} label="Articles per hour, last 24 hours" className="card__spark" />
        </span>
        {story.top_sources.length > 0 && (
          <span className="card__sources">{story.top_sources.map(sourceName).join(" · ")}</span>
        )}
      </span>
    </button>
  );
}

function Ticker() {
  const ticker = useApp((s) => s.ticker);
  const select = useApp((s) => s.select);
  if (ticker.length === 0) return null;
  return (
    <section className="ticker" aria-label="Live activity">
      <h3 className="ticker__title">Live activity</h3>
      <ul aria-live="polite" aria-relevant="additions">
        {ticker.slice(0, 4).map((item) => (
          <TickerRow key={item.id} item={item} onSelect={() => select(item.storyId)} />
        ))}
      </ul>
    </section>
  );
}

function TickerRow({ item, onSelect }: { item: TickerItem; onSelect: () => void }) {
  const Icon = EVENT_ICON[item.kind as keyof typeof EVENT_ICON] ?? Newspaper;
  const tone = item.kind === "created" ? "birth" : item.kind === "article_added" ? "article" : "lineage";
  const verb = { created: "New story", article_added: "New article", merged: "Merged", split: "Split" }[
    item.kind as "created"
  ];
  return (
    <li>
      <button className="ticker__row" onClick={onSelect}>
        <Icon size={13} className={`ev-${tone}`} aria-hidden />
        <span className="ticker__verb">{verb}</span>
        <span className="ticker__headline">{item.headline}</span>
      </button>
    </li>
  );
}
