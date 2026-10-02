import { GitFork, Newspaper, Sparkle } from "lucide-react";

/** Explains size, color and rings. Every color is paired with an icon and a word. */
export function Legend() {
  return (
    <aside className="legend" aria-label="Graph legend">
      <div className="legend__row">
        <span className="legend__sizes" aria-hidden>
          <i style={{ width: 8, height: 8 }} />
          <i style={{ width: 14, height: 14 }} />
          <i style={{ width: 22, height: 22 }} />
        </span>
        <span>Size: articles</span>
      </div>
      <div className="legend__row">
        <span className="legend__ramp" aria-hidden />
        <span className="legend__ramp-labels">
          <span>now</span>
          <span>6h</span>
          <span>24h+</span>
        </span>
      </div>
      <ul className="legend__events">
        <li>
          <Sparkle size={13} className="ev-birth" aria-hidden />
          New story
        </li>
        <li>
          <Newspaper size={13} className="ev-article" aria-hidden />
          New article
        </li>
        <li>
          <GitFork size={13} className="ev-lineage" aria-hidden />
          Split / merge
        </li>
      </ul>
    </aside>
  );
}
