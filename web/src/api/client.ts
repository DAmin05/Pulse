import type {
  GraphResponse,
  PipelineResponse,
  Replay,
  SearchResponse,
  StatsResponse,
  StoriesResponse,
  StoryResponse,
  TimelineBucket,
} from "./types";

export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
  }
}

async function get<T>(path: string, params: Record<string, string | number | undefined> = {}): Promise<T> {
  const query = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== "") query.set(k, String(v));
  }
  const qs = query.toString();
  const res = await fetch(`/api${path}${qs ? `?${qs}` : ""}`);
  if (!res.ok) {
    const body = await res.json().catch(() => ({}));
    throw new ApiError(res.status, body.error ?? res.statusText);
  }
  return res.json() as Promise<T>;
}

/** `at`: a past log position; `undefined` means live. */
export const api = {
  stories: (p: { at?: number; lang?: string; sort?: string; limit?: number; min_sources?: number }) =>
    get<StoriesResponse>("/stories", p),
  story: (id: string, at?: number) => get<StoryResponse>(`/stories/${encodeURIComponent(id)}`, { at }),
  graph: (p: { at?: number; limit?: number; min_sources?: number; min_similarity?: number }) =>
    get<GraphResponse>("/graph", p),
  search: (q: string, limit = 60) => get<SearchResponse>("/search", { q, limit }),
  timeline: (buckets = 120) => get<{ buckets: TimelineBucket[] }>("/timeline", { buckets }),
  stats: () => get<StatsResponse>("/stats"),
  pipeline: () => get<PipelineResponse>("/pipeline"),
  replay: (id: string) => get<Replay>(`/replays/${encodeURIComponent(id)}`),
  startReplay: async (body: { from: number; to: number }): Promise<Replay> => {
    const res = await fetch("/api/replays", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    if (!res.ok) {
      const err = await res.json().catch(() => ({}));
      throw new ApiError(res.status, err.error ?? res.statusText);
    }
    return res.json() as Promise<Replay>;
  },
};
