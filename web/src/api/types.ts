// Response shapes of the Query API (crates/query-api, crates/pulse-store/src/reader.rs).

export interface Position {
  /** Offset in articles.embedded the view reflects. */
  at: number;
  /** Pipeline time at that offset. */
  time: string | null;
  latest: number;
  live: boolean;
}

export interface StoryCard {
  id: string;
  headline: string;
  headline_article_id: string;
  lang: string;
  langs: string[];
  article_count: number;
  source_count: number;
  top_sources: string[];
  created_at: string;
  updated_at: string;
  closed_at: string | null;
  close_reason: string | null;
  parent_ids: string[];
  merged_from: string[];
  merged_into: string | null;
  /** Articles per hour of publication, oldest first (24 buckets). */
  activity: number[];
}

export interface StoriesResponse extends Position {
  stories: StoryCard[];
}

export interface ArticleView {
  id: string;
  title: string;
  summary: string;
  url: string;
  source_id: string;
  lang: string;
  published_at: string;
  is_duplicate: boolean;
  late: boolean;
  score: number;
}

export interface StoryRef {
  id: string;
  headline: string;
}

export interface Lineage {
  parents: StoryRef[];
  children: StoryRef[];
  merged_from: StoryRef[];
  merged_into: StoryRef | null;
}

export interface StoryDetail extends StoryCard {
  articles: ArticleView[];
  lineage: Lineage;
  events: LiveEvent[];
}

export interface StoryResponse extends Position {
  story: StoryDetail;
}

export interface GraphEdge {
  source: string;
  target: string;
  kind: "similar" | "split";
  similarity: number | null;
}

export interface GraphResponse extends Position {
  nodes: StoryCard[];
  edges: GraphEdge[];
  lineage: LiveEvent[];
}

export interface SearchHit {
  article: ArticleView;
  similarity: number;
  story_id: string | null;
}

export interface SearchResult {
  story: StoryCard;
  best_similarity: number;
  strong: boolean;
  hits: SearchHit[];
}

export interface SearchResponse {
  query: string;
  model_version: string;
  results: SearchResult[];
  unassigned: SearchHit[];
}

export interface TimelineBucket {
  start: string;
  offset: number;
  articles: number;
  created: number;
  splits: number;
  merges: number;
  closed: number;
}

export interface Totals {
  articles: number;
  stories_open: number;
  stories_total: number;
  languages: number;
  sources: number;
  splits: number;
  merges: number;
  late_articles: number;
  duplicates: number;
  first_fetched_at: string | null;
  last_fetched_at: string | null;
  latest_offset: number;
}

export interface StatsResponse {
  totals: Totals;
  topics: Record<string, number | null>;
  sink: { topic: string; partition: number; next_offset: number; lag: number | null }[];
}

export interface PipelineMetric {
  key: string;
  label: string;
  unit: string;
  value: number | null;
  series: [number, number][];
}

export interface PipelineStage {
  stage: string;
  group: string;
  topic: string;
  position?: number | null;
  end?: number | null;
  lag: number | null;
}

export interface PipelineResponse {
  generated_at: string;
  window_seconds: number;
  metrics: PipelineMetric[];
  stages: PipelineStage[];
}

export interface ReplayReport {
  from: number;
  to: number;
  clamped: boolean;
  snapshot_offset: number | null;
  fingerprint: string;
  warmup_inputs: number;
  inputs: number;
  events: {
    original: number;
    replayed: number;
    matched: number;
    original_hash: string;
    replayed_hash: string;
    identical: boolean;
    first_divergence: { position: number; original: string | null; replayed: string | null } | null;
  };
  late: { original: number; replayed: number; identical: boolean };
  identical: boolean;
  output_topic: string | null;
  seconds: number;
}

export interface Replay {
  id: string;
  status: "queued" | "running" | "done" | "failed";
  from_offset: number;
  to_offset: number | null;
  output_topic: string | null;
  requested_at: string;
  started_at: string | null;
  finished_at: string | null;
  identical: boolean | null;
  report: ReplayReport | null;
  error: string | null;
}

/** A committed story event, as streamed by /api/stream and stored in story_events. */
export type LiveEvent = {
  event_id: string;
  offset: number;
  seq: number;
  event_time: number;
  watermark: number | null;
  story_id: string;
} & (
  | { kind: "created"; seed_article_id: string; headline: string; lang: string; parent_ids: string[] }
  | {
      kind: "article_added";
      article_id: string;
      score: number;
      is_duplicate: boolean;
      duplicate_of: string;
      late: boolean;
    }
  | {
      kind: "updated";
      headline: string;
      headline_article_id: string;
      article_count: number;
      source_count: number;
      langs: string[];
    }
  | { kind: "split"; children: { story_id: string; article_count: number }[] }
  | { kind: "merged"; source_ids: string[] }
  | { kind: "closed"; reason: "idle" | "merged" | "split" | "unspecified" }
);

export type EventKind = LiveEvent["kind"];

// --- Listen ------------------------------------------------------------------

export interface ListenLanguage {
  code: string;
  name: string;
  native: string;
  bcp47: string;
}

export interface ListenVoice {
  id: string;
  name: string;
  description: string | null;
}

export interface ListenCapabilities {
  languages: ListenLanguage[];
  translation: { provider: string; budget: { used_this_month: number; monthly_limit: number } } | null;
  speech: {
    provider: "elevenlabs";
    model: string;
    default_voice: string;
    voices: ListenVoice[];
    budget: {
      used_today: number;
      daily_limit: number;
      account: { used: number; limit: number; resets_at: number | null } | null;
    };
  } | null;
  max_chars: number;
}

export interface BriefingSegment {
  kind: "headline" | "coverage" | "report";
  text: string;
  original_lang: string;
  translated: boolean;
  source: { article_id: string; name: string; url: string; lang: string } | null;
}

export interface Briefing {
  story_id: string;
  lang: string;
  bcp47: string;
  text: string;
  segments: BriefingSegment[];
  translation: { provider: string; segments: number; cached_segments: number; characters: number } | null;
  audio: {
    url: string;
    voice: string;
    voice_name: string | null;
    model: string;
    /** [UTF-16 offset into `text`, seconds] per word. */
    words: [number, number][];
    duration: number;
    characters: number;
    cached: boolean;
  } | null;
  fallback: { reason: string; message: string } | null;
  notice: string | null;
}
