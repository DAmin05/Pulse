import { keepPreviousData, useQuery } from "@tanstack/react-query";

import { useApp, viewAt } from "../live/store";
import { api } from "./client";

const LIVE_REFRESH_MS = 30_000;

export function useStories(limit = 40) {
  const view = useApp((s) => s.view);
  const lang = useApp((s) => s.lang);
  const at = viewAt(view);
  return useQuery({
    queryKey: ["stories", at ?? "live", lang, limit],
    queryFn: () => api.stories({ at, lang: lang ?? undefined, limit, sort: "sources", min_sources: 2 }),
    placeholderData: keepPreviousData,
  });
}

export function useGraph() {
  const view = useApp((s) => s.view);
  const at = viewAt(view);
  return useQuery({
    queryKey: ["graph", at ?? "live"],
    queryFn: () => api.graph({ at, limit: 140, min_sources: 2, min_similarity: 0.35 }),
    // Live: periodic reconcile (similarity edges, counts); events animate in between.
    refetchInterval: at === undefined ? LIVE_REFRESH_MS : false,
    placeholderData: keepPreviousData,
  });
}

export function useStory(id: string | null) {
  const view = useApp((s) => s.view);
  const at = viewAt(view);
  return useQuery({
    queryKey: ["story", id, at ?? "live"],
    queryFn: () => api.story(id!, at),
    enabled: id !== null,
    placeholderData: keepPreviousData,
  });
}

export function useTimeline() {
  return useQuery({
    queryKey: ["timeline"],
    queryFn: () => api.timeline(120),
    refetchInterval: LIVE_REFRESH_MS,
  });
}

export function useStats() {
  return useQuery({ queryKey: ["stats"], queryFn: api.stats, refetchInterval: 10_000 });
}

export function usePipeline(enabled: boolean) {
  return useQuery({
    queryKey: ["pipeline"],
    queryFn: api.pipeline,
    enabled,
    refetchInterval: enabled ? 5_000 : false,
  });
}

export function useSearch(q: string) {
  const query = q.trim();
  return useQuery({
    queryKey: ["search", query],
    queryFn: () => api.search(query),
    enabled: query.length >= 2,
    staleTime: 60_000,
    placeholderData: keepPreviousData,
  });
}
