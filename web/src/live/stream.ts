import { useQueryClient } from "@tanstack/react-query";
import { useEffect } from "react";

import type { LiveEvent } from "../api/types";
import { sourceName } from "../lib/format";
import { graphModel, useApp } from "./store";

const REFRESH_LISTS_MS = 4000;

/**
 * Connects to the live event stream for the app's lifetime. EventSource
 * reconnects by itself and sends Last-Event-ID, which the API uses to replay
 * exactly what was missed. While viewing the past, events are counted (for a
 * "back to live" badge) but not applied.
 */
export function useLiveStream() {
  const queryClient = useQueryClient();

  useEffect(() => {
    const source = new EventSource("/api/stream");
    const { setConnection, pushTicker, bumpGraph, noteMissed } = useApp.getState();
    let dirty = false;

    source.onopen = () => setConnection("live");
    source.onerror = () => setConnection("reconnecting");

    source.addEventListener("resync", () => {
      // Too far behind to replay: take a fresh snapshot.
      queryClient.invalidateQueries({ queryKey: ["graph"] });
      queryClient.invalidateQueries({ queryKey: ["stories"] });
    });

    source.addEventListener("story", (msg) => {
      const ev = JSON.parse((msg as MessageEvent<string>).data) as LiveEvent;
      if (useApp.getState().view.mode !== "live") {
        noteMissed();
        return;
      }
      const touched = graphModel.applyEvent(ev, performance.now());
      dirty = true;
      if (touched.length) bumpGraph();
      const item = tickerItem(ev);
      if (item) pushTicker(item);
    });

    // Lists and counters refresh in the background while events flow.
    const timer = window.setInterval(() => {
      if (!dirty) return;
      dirty = false;
      queryClient.invalidateQueries({ queryKey: ["stories"] });
      queryClient.invalidateQueries({ queryKey: ["stats"] });
      const selected = useApp.getState().selected;
      if (selected) queryClient.invalidateQueries({ queryKey: ["story", selected] });
    }, REFRESH_LISTS_MS);

    return () => {
      source.close();
      window.clearInterval(timer);
    };
  }, [queryClient]);
}

function headlineOf(id: string): string | undefined {
  return graphModel.nodes.get(id)?.headline;
}

/** Events worth showing in the ticker: births, lineage, and growth of visible stories. */
function tickerItem(ev: LiveEvent): ReturnType<typeof useApp.getState>["ticker"][number] | null {
  const at = Date.now();
  const id = `${ev.offset}:${ev.seq}`;
  switch (ev.kind) {
    case "updated":
      // Promotions into the graph show up as births.
      if (ev.source_count === 2 && graphModel.nodes.get(ev.story_id)?.bornAt) {
        return { id, kind: "created", storyId: ev.story_id, headline: ev.headline, detail: "now covered by 2 sources", at };
      }
      return null;
    case "article_added": {
      const headline = headlineOf(ev.story_id);
      if (!headline) return null;
      const node = graphModel.nodes.get(ev.story_id);
      return {
        id,
        kind: "article_added",
        storyId: ev.story_id,
        headline,
        detail: ev.is_duplicate
          ? "syndicated copy"
          : node
            ? `${node.articleCount} articles · ${node.topSources[0] ? sourceName(node.topSources[0]) : ""}`.replace(/ · $/, "")
            : undefined,
        at,
      };
    }
    case "merged":
      return {
        id,
        kind: "merged",
        storyId: ev.story_id,
        headline: headlineOf(ev.story_id) ?? "A story",
        detail: `absorbed ${ev.source_ids.length === 1 ? "a related story" : `${ev.source_ids.length} stories`}`,
        at,
      };
    case "split":
      return {
        id,
        kind: "split",
        storyId: ev.story_id,
        headline: headlineOf(ev.story_id) ?? "A story",
        detail: `split into ${ev.children.length} narratives`,
        at,
      };
    default:
      return null;
  }
}
