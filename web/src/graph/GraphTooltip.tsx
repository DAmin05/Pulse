import { Sparkline } from "../components/Sparkline";
import { languageName, plural, relativeTime, sourceName } from "../lib/format";
import { useReferenceTime } from "../lib/time";
import { graphModel, useApp } from "../live/store";

interface Props {
  id: string;
  x: number;
  y: number;
}

/** Hover card for a graph node, kept inside the canvas bounds. */
export function GraphTooltip({ id, x, y }: Props) {
  useApp((s) => s.graphVersion); // re-render as the node's counts change
  const reference = useReferenceTime();
  const node = graphModel.nodes.get(id);
  if (!node) return null;
  const flipX = x > window.innerWidth * 0.55;

  return (
    <div
      className="tooltip"
      style={{
        left: flipX ? undefined : x + 16,
        right: flipX ? `calc(100% - ${x - 16}px)` : undefined,
        top: Math.max(8, y - 12),
      }}
      role="tooltip"
    >
      <p className="tooltip__headline">{node.headline}</p>
      <p className="tooltip__meta">
        <span className="num">{plural(node.sourceCount, "source")}</span>
        <span aria-hidden>·</span>
        <span className="num">{plural(node.articleCount, "article")}</span>
        <span aria-hidden>·</span>
        <span>{relativeTime(node.lastActivity, reference)}</span>
      </p>
      {node.activity.length > 1 && (
        <Sparkline values={node.activity} width={236} height={30} label="Articles per hour, last 24 hours" />
      )}
      <p className="tooltip__langs">{node.langs.slice(0, 6).map(languageName).join(" · ")}{node.langs.length > 6 ? ` +${node.langs.length - 6}` : ""}</p>
      {node.topSources.length > 0 && (
        <p className="tooltip__sources">{node.topSources.map(sourceName).join(", ")}</p>
      )}
    </div>
  );
}
