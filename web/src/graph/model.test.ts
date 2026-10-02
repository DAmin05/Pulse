import { describe, expect, it } from "vitest";

import type { GraphResponse, LiveEvent, StoryCard } from "../api/types";
import { GraphModel, LEAVE_MS } from "./model";

const base = { event_id: "e", offset: 1, seq: 0, event_time: 1_000, watermark: null };

function card(id: string, sources: number, articles = sources, parents: string[] = []): StoryCard {
  return {
    id,
    headline: `headline ${id}`,
    headline_article_id: "a",
    lang: "en",
    langs: ["en"],
    article_count: articles,
    source_count: sources,
    top_sources: [],
    created_at: "2026-10-01T00:00:00Z",
    updated_at: "2026-10-01T00:00:00Z",
    closed_at: null,
    close_reason: null,
    parent_ids: parents,
    merged_from: [],
    merged_into: null,
    activity: [],
  };
}

function snapshot(nodes: StoryCard[]): GraphResponse {
  return { at: 1, time: null, latest: 1, live: true, nodes, edges: [], lineage: [] };
}

const created = (id: string, parent_ids: string[] = []): LiveEvent => ({
  ...base,
  kind: "created",
  story_id: id,
  seed_article_id: "a",
  headline: `new ${id}`,
  lang: "es",
  parent_ids,
});

const updated = (id: string, sources: number, articles = sources): LiveEvent => ({
  ...base,
  kind: "updated",
  story_id: id,
  headline: `updated ${id}`,
  headline_article_id: "a",
  article_count: articles,
  source_count: sources,
  langs: ["en", "es"],
});

const LATER = Date.parse("2026-10-01T01:00:00Z"); // after the cards' updated_at

const added = (id: string): LiveEvent => ({
  ...base,
  event_time: LATER,
  kind: "article_added",
  story_id: id,
  article_id: "x",
  score: 1,
  is_duplicate: false,
  duplicate_of: "",
  late: false,
});

describe("GraphModel", () => {
  it("promotes a story once it reaches enough sources, with a birth pulse", () => {
    const m = new GraphModel({ minSources: 2, maxNodes: 100 });
    m.applyEvent(created("s1"), 0);
    expect(m.nodes.size).toBe(0);
    expect(m.applyEvent(updated("s1", 2, 3), 10)).toEqual(["s1"]);
    const node = m.nodes.get("s1")!;
    expect(node.headline).toBe("updated s1");
    expect(node.articleCount).toBe(3);
    expect(node.bornAt).toBe(10);
    expect(node.pulses.map((p) => p.kind)).toEqual(["birth"]);
  });

  it("grows and pulses visible stories as articles arrive", () => {
    const m = new GraphModel({ minSources: 2, maxNodes: 100 });
    m.applySnapshot(snapshot([card("s1", 3)]), 0, true);
    expect(m.nodes.get("s1")!.bornAt).toBeNull(); // initial load doesn't animate births
    m.applyEvent(added("s1"), 5);
    const node = m.nodes.get("s1")!;
    expect(node.articleCount).toBe(4);
    expect(node.lastActivity).toBe(LATER);
    expect(node.pulses).toEqual([{ kind: "article", at: 5 }]);
  });

  it("merges: the source flies into the target, which pulses", () => {
    const m = new GraphModel({ minSources: 2, maxNodes: 100 });
    m.applySnapshot(snapshot([card("big", 9), card("small", 3)]), 0, true);
    const touched = m.applyEvent({ ...base, kind: "merged", story_id: "big", source_ids: ["small"] }, 100);
    expect(touched.sort()).toEqual(["big", "small"]);
    expect(m.nodes.get("small")!.leaving).toEqual({ kind: "merged", at: 100, into: "big" });
    expect(m.nodes.get("big")!.pulses[0]!.kind).toBe("lineage");
    expect(m.flashes).toHaveLength(1);
    expect(m.live().map((n) => n.id)).toEqual(["big"]);
    m.prune(100 + LEAVE_MS + 1);
    expect([...m.nodes.keys()]).toEqual(["big"]);
  });

  it("splits: the parent departs and children appear at its position", () => {
    const m = new GraphModel({ minSources: 2, maxNodes: 100 });
    m.applySnapshot(snapshot([card("p", 8)]), 0, true);
    Object.assign(m.nodes.get("p")!, { x: 500, y: -300 });
    m.applyEvent(
      {
        ...base,
        kind: "split",
        story_id: "p",
        children: [
          { story_id: "c1", article_count: 5 },
          { story_id: "c2", article_count: 3 },
        ],
      },
      50,
    );
    expect(m.nodes.get("p")!.leaving?.kind).toBe("split");
    for (const c of ["c1", "c2"]) {
      m.applyEvent(created(c, ["p"]), 51);
      m.applyEvent(updated(c, 3), 52);
      const child = m.nodes.get(c)!;
      expect(Math.abs(child.x! - 500)).toBeLessThan(20);
      expect(Math.abs(child.y! + 300)).toBeLessThan(20);
      expect(child.parentIds).toEqual(["p"]);
    }
    expect(m.flashes.map((f) => f.to)).toEqual(["c1", "c2"]);
  });

  it("idle closes fade nodes out; merge/split closes are ignored", () => {
    const m = new GraphModel({ minSources: 2, maxNodes: 100 });
    m.applySnapshot(snapshot([card("a", 2), card("b", 2)]), 0, true);
    m.applyEvent({ ...base, kind: "closed", story_id: "a", reason: "merged" }, 1);
    expect(m.nodes.get("a")!.leaving).toBeNull();
    m.applyEvent({ ...base, kind: "closed", story_id: "b", reason: "idle" }, 1);
    expect(m.nodes.get("b")!.leaving?.kind).toBe("closed");
  });

  it("snapshots keep positions, add newcomers and retire the missing", () => {
    const m = new GraphModel({ minSources: 2, maxNodes: 100 });
    m.applySnapshot(snapshot([card("a", 2), card("b", 2)]), 0, true);
    Object.assign(m.nodes.get("a")!, { x: 42, y: 7 });
    m.applySnapshot(snapshot([card("a", 5), card("c", 2)]), 100);
    const a = m.nodes.get("a")!;
    expect([a.x, a.y, a.articleCount]).toEqual([42, 7, 5]);
    expect(m.nodes.get("c")!.bornAt).toBe(100);
    expect(m.nodes.get("b")!.leaving?.kind).toBe("evicted");
  });

  it("caps the node count by evicting the least-covered stories", () => {
    const m = new GraphModel({ minSources: 2, maxNodes: 2 });
    m.applySnapshot(snapshot([card("a", 9), card("b", 2)]), 0, true);
    m.applyEvent(created("c"), 1);
    m.applyEvent(updated("c", 4), 2);
    expect(m.live().map((n) => n.id).sort()).toEqual(["a", "c"]);
  });
});
