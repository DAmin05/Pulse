// The story graph's state: which stories are on screen, their sizes, and the
// animation state the renderer draws (births, pulses, departures, lineage
// flashes). Pure TypeScript, no DOM: fed by graph snapshots and live events,
// read by the canvas every frame.

import type { GraphEdge, GraphResponse, LiveEvent, StoryCard } from "../api/types";

export type PulseKind = "birth" | "article" | "lineage";

export interface Pulse {
  kind: PulseKind;
  at: number;
}

export interface Leaving {
  kind: "closed" | "merged" | "split" | "evicted";
  at: number;
  /** For merges: the story this one flies into. */
  into?: string;
}

export interface GNode {
  id: string;
  headline: string;
  articleCount: number;
  sourceCount: number;
  langs: string[];
  topSources: string[];
  activity: number[];
  /** Event time (ms) of the latest article: drives the recency color. */
  lastActivity: number;
  bornAt: number | null;
  pulses: Pulse[];
  leaving: Leaving | null;
  parentIds: string[];
  // Simulation fields (owned by d3-force).
  x?: number;
  y?: number;
  vx?: number;
  vy?: number;
  fx?: number | null;
  fy?: number | null;
}

export interface Flash {
  from: string;
  to: string;
  at: number;
  kind: "split" | "merge";
}

/** A story we've heard of but don't draw (yet): too few sources. */
interface Known {
  headline: string;
  articleCount: number;
  sourceCount: number;
  langs: string[];
  lastActivity: number;
  parentIds: string[];
}

export interface ModelOptions {
  /** Stories need this many sources to appear. */
  minSources: number;
  maxNodes: number;
}

export const PULSE_MS = 1600;
export const LEAVE_MS = 900;
export const FLASH_MS = 5000;
export const BIRTH_MS = 700;

export class GraphModel {
  nodes = new Map<string, GNode>();
  edges: GraphEdge[] = [];
  flashes: Flash[] = [];
  /** Bumped on topology changes (nodes added/removed, edges replaced). */
  topology = 0;
  private known = new Map<string, Known>();

  constructor(private opts: ModelOptions) {}

  setOptions(opts: Partial<ModelOptions>) {
    this.opts = { ...this.opts, ...opts };
  }

  /** Visible (not departing) nodes. */
  live(): GNode[] {
    return [...this.nodes.values()].filter((n) => !n.leaving);
  }

  /**
   * Reconciles with a graph snapshot: existing nodes keep their positions and
   * animate to new sizes, new ones are born (unless `initial`), and nodes no
   * longer present depart.
   */
  applySnapshot(snapshot: GraphResponse, now: number, initial = false) {
    const ids = new Set(snapshot.nodes.map((n) => n.id));
    for (const card of snapshot.nodes) {
      const existing = this.nodes.get(card.id);
      if (existing) {
        Object.assign(existing, fromCard(card), { leaving: null });
      } else {
        const node: GNode = { ...fromCard(card), bornAt: initial ? null : now, pulses: [], leaving: null };
        this.placeNear(node, card.parent_ids);
        this.nodes.set(card.id, node);
      }
      this.known.set(card.id, {
        headline: card.headline,
        articleCount: card.article_count,
        sourceCount: card.source_count,
        langs: card.langs,
        lastActivity: Date.parse(card.updated_at),
        parentIds: card.parent_ids,
      });
    }
    for (const node of this.nodes.values()) {
      if (!ids.has(node.id) && !node.leaving) {
        node.leaving = { kind: "evicted", at: now };
      }
    }
    this.edges = snapshot.edges.filter((e) => ids.has(e.source) && ids.has(e.target));
    this.topology++;
  }

  /** Applies one live story event. Returns the ids of nodes it visibly touched. */
  applyEvent(ev: LiveEvent, now: number): string[] {
    switch (ev.kind) {
      case "created": {
        this.known.set(ev.story_id, {
          headline: ev.headline,
          articleCount: 1,
          sourceCount: 1,
          langs: [ev.lang],
          lastActivity: ev.event_time,
          parentIds: ev.parent_ids,
        });
        return [];
      }
      case "article_added": {
        const k = this.known.get(ev.story_id);
        if (k) {
          k.articleCount++;
          k.lastActivity = Math.max(k.lastActivity, ev.event_time);
        }
        const node = this.nodes.get(ev.story_id);
        if (node && !node.leaving) {
          node.articleCount++;
          node.lastActivity = Math.max(node.lastActivity, ev.event_time);
          node.pulses.push({ kind: "article", at: now });
          return [node.id];
        }
        return [];
      }
      case "updated": {
        const k = this.known.get(ev.story_id) ?? {
          headline: ev.headline,
          articleCount: ev.article_count,
          sourceCount: ev.source_count,
          langs: ev.langs,
          lastActivity: ev.event_time,
          parentIds: [],
        };
        Object.assign(k, {
          headline: ev.headline,
          articleCount: ev.article_count,
          sourceCount: ev.source_count,
          langs: ev.langs,
          lastActivity: Math.max(k.lastActivity, ev.event_time),
        });
        this.known.set(ev.story_id, k);
        const node = this.nodes.get(ev.story_id);
        if (node && !node.leaving) {
          Object.assign(node, {
            headline: ev.headline,
            articleCount: ev.article_count,
            sourceCount: ev.source_count,
            langs: ev.langs,
            lastActivity: k.lastActivity,
          });
          return [node.id];
        }
        if (k.sourceCount >= this.opts.minSources) {
          this.promote(ev.story_id, k, now);
          return [ev.story_id];
        }
        return [];
      }
      case "merged": {
        const touched: string[] = [];
        const target = this.nodes.get(ev.story_id);
        for (const source of ev.source_ids) {
          this.known.delete(source);
          const node = this.nodes.get(source);
          if (node && !node.leaving) {
            node.leaving = { kind: "merged", at: now, into: ev.story_id };
            touched.push(source);
            if (target) this.flashes.push({ from: source, to: ev.story_id, at: now, kind: "merge" });
          }
        }
        if (target && !target.leaving) {
          target.pulses.push({ kind: "lineage", at: now });
          touched.push(target.id);
        }
        if (touched.length) this.topology++;
        return touched;
      }
      case "split": {
        this.known.delete(ev.story_id);
        const parent = this.nodes.get(ev.story_id);
        if (parent && !parent.leaving) {
          parent.leaving = { kind: "split", at: now };
          for (const child of ev.children) {
            this.flashes.push({ from: ev.story_id, to: child.story_id, at: now, kind: "split" });
          }
          this.topology++;
          return [parent.id];
        }
        return [];
      }
      case "closed": {
        if (ev.reason !== "idle") return []; // merges/splits handled above
        this.known.delete(ev.story_id);
        const node = this.nodes.get(ev.story_id);
        if (node && !node.leaving) {
          node.leaving = { kind: "closed", at: now };
          this.topology++;
          return [node.id];
        }
        return [];
      }
    }
  }

  /** Drops finished animations and departed nodes. Returns true if topology changed. */
  prune(now: number): boolean {
    let changed = false;
    for (const [id, node] of this.nodes) {
      node.pulses = node.pulses.filter((p) => now - p.at < PULSE_MS);
      if (node.leaving && now - node.leaving.at > LEAVE_MS) {
        this.nodes.delete(id);
        changed = true;
      }
    }
    this.flashes = this.flashes.filter((f) => now - f.at < FLASH_MS);
    if (changed) {
      const ids = new Set(this.nodes.keys());
      this.edges = this.edges.filter((e) => ids.has(e.source) && ids.has(e.target));
      this.topology++;
    }
    return changed;
  }

  private promote(id: string, k: Known, now: number) {
    const node: GNode = {
      id,
      headline: k.headline,
      articleCount: k.articleCount,
      sourceCount: k.sourceCount,
      langs: k.langs,
      topSources: [],
      activity: [],
      lastActivity: k.lastActivity,
      bornAt: now,
      pulses: [{ kind: "birth", at: now }],
      leaving: null,
      parentIds: k.parentIds,
    };
    this.placeNear(node, k.parentIds);
    this.nodes.set(id, node);
    this.enforceCap(now);
    this.topology++;
  }

  /** New nodes start at their parent (split children burst out of it) or near the center. */
  private placeNear(node: GNode, parentIds: string[]) {
    const parent = parentIds.map((p) => this.nodes.get(p)).find((p) => p?.x !== undefined);
    const jitter = () => (Math.random() - 0.5) * 24;
    if (parent && parent.x !== undefined && parent.y !== undefined) {
      node.x = parent.x + jitter();
      node.y = parent.y + jitter();
    } else {
      const angle = Math.random() * Math.PI * 2;
      const radius = 160 + Math.random() * 120;
      node.x = Math.cos(angle) * radius;
      node.y = Math.sin(angle) * radius;
    }
  }

  /** Keeps the graph readable: evicts the least-covered, least-recent stories. */
  private enforceCap(now: number) {
    const live = this.live();
    const excess = live.length - this.opts.maxNodes;
    if (excess <= 0) return;
    live
      .sort((a, b) => a.sourceCount - b.sourceCount || a.lastActivity - b.lastActivity)
      .slice(0, excess)
      .forEach((n) => (n.leaving = { kind: "evicted", at: now }));
  }
}

function fromCard(card: StoryCard) {
  return {
    id: card.id,
    headline: card.headline,
    articleCount: card.article_count,
    sourceCount: card.source_count,
    langs: card.langs,
    topSources: card.top_sources,
    activity: card.activity,
    lastActivity: Date.parse(card.updated_at),
    parentIds: card.parent_ids,
  };
}

/** Node radius in graph units: area grows with article count. */
export function radius(articleCount: number): number {
  return Math.min(56, 3 + 4.4 * Math.sqrt(Math.max(1, articleCount)));
}
