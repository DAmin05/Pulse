import { forceCollide, forceLink, forceManyBody, forceRadial, forceSimulation, forceX, forceY } from "d3-force";
import type { Simulation, SimulationLinkDatum } from "d3-force";
import { select } from "d3-selection";
import { zoom, zoomIdentity, type ZoomBehavior, type ZoomTransform } from "d3-zoom";
import { Maximize2, Minus, Plus } from "lucide-react";
import { useEffect, useRef, useState } from "react";

import { useGraph } from "../api/hooks";
import { graphModel, useApp } from "../live/store";
import { readPalette, recencyColor, withAlpha, type Palette } from "./colors";
import { BIRTH_MS, FLASH_MS, LEAVE_MS, PULSE_MS, radius, type GNode } from "./model";
import { GraphTooltip } from "./GraphTooltip";

interface Link extends SimulationLinkDatum<GNode> {
  similarity: number;
  kind: string;
}

type Drawn = GNode & { dr?: number; ring?: number; prominence?: number };

/** Radial layout: the most widely covered stories at the center, minor ones outside. */
function assignRings(nodes: Drawn[]) {
  const ranked = [...nodes].sort((a, b) => b.sourceCount - a.sourceCount || b.articleCount - a.articleCount);
  ranked.forEach((n, i) => {
    const p = ranked.length > 1 ? i / (ranked.length - 1) : 0;
    n.ring = 30 + 380 * Math.pow(p, 0.7);
    // The long tail recedes so the eye lands on what most outlets cover.
    n.prominence = p < 0.2 ? 1 : 1 - 0.5 * ((p - 0.2) / 0.8);
  });
}

const LABELS = 12;
const reducedMotion = () => window.matchMedia("(prefers-reduced-motion: reduce)").matches;
const easeOutBack = (t: number) => 1 + 2.2 * Math.pow(t - 1, 3) + 1.2 * Math.pow(t - 1, 2);
const easeInOut = (t: number) => (t < 0.5 ? 2 * t * t : 1 - Math.pow(-2 * t + 2, 2) / 2);
const clamp01 = (t: number) => Math.max(0, Math.min(1, t));

/** Transform that frames every visible node in a w×h viewport. */
function fitTransform(nodes: GNode[], w: number, h: number): ZoomTransform | null {
  const placed = nodes.filter((n) => n.x !== undefined && !n.leaving);
  if (!placed.length) return null;
  const pad = 70;
  const xs = placed.map((n) => n.x!);
  const ys = placed.map((n) => n.y!);
  const [minX, maxX, minY, maxY] = [Math.min(...xs), Math.max(...xs), Math.min(...ys), Math.max(...ys)];
  const k = Math.min(2, Math.min(w / (maxX - minX + 2 * pad), h / (maxY - minY + 2 * pad)));
  return zoomIdentity.translate(w / 2 - ((minX + maxX) / 2) * k, h / 2 - ((minY + maxY) / 2) * k).scale(k);
}

function truncate(text: string, max: number) {
  return text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text;
}

export function GraphCanvas() {
  const containerRef = useRef<HTMLDivElement>(null);
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const transform = useRef<ZoomTransform>(zoomIdentity);
  const zoomBehavior = useRef<ZoomBehavior<HTMLCanvasElement, unknown> | null>(null);
  const firstSnapshot = useRef(true);
  /** Frame the graph once its first layout has settled. */
  const pendingFit = useRef(true);
  const hoveredRef = useRef<string | null>(null);
  const [hover, setHover] = useState<{ id: string; x: number; y: number } | null>(null);

  const graph = useGraph();
  const selected = useApp((s) => s.selected);
  const bumpGraph = useApp((s) => s.bumpGraph);
  const view = useApp((s) => s.view);
  // The render loop reads the latest selection/view through refs.
  const selectedRef = useRef(selected);
  const viewRef = useRef(view);
  useEffect(() => {
    selectedRef.current = selected;
    viewRef.current = view;
  }, [selected, view]);
  const empty = (graph.data?.nodes.length ?? 0) === 0;

  // Snapshots (initial load, periodic live reconcile, time travel) reconcile into the model.
  useEffect(() => {
    if (!graph.data) return;
    graphModel.applySnapshot(graph.data, performance.now(), firstSnapshot.current);
    firstSnapshot.current = false;
    bumpGraph();
  }, [graph.data, bumpGraph]);

  // Simulation + render loop for the component's lifetime.
  useEffect(() => {
    const canvas = canvasRef.current!;
    const container = containerRef.current!;
    const ctx = canvas.getContext("2d")!;
    let palette: Palette = readPalette();
    let width = 0;
    let height = 0;
    let dpr = 1;

    const sim: Simulation<GNode, Link> = forceSimulation<GNode, Link>()
      .force("charge", forceManyBody<GNode>().strength((n) => -6 - radius(n.articleCount) * 2.5))
      .force("collide", forceCollide<GNode>((n) => radius(n.articleCount) + 5).strength(0.9).iterations(2))
      .force(
        "link",
        forceLink<GNode, Link>()
          .id((n) => n.id)
          .distance((l) => 34 + (1 - l.similarity) * 110)
          .strength((l) => 0.12 + 0.5 * l.similarity),
      )
      .force("radial", forceRadial<Drawn>((n) => n.ring ?? 300, 0, 0).strength(0.09))
      .force("x", forceX<GNode>(0).strength(0.008))
      .force("y", forceY<GNode>(0).strength(0.012))
      .alphaDecay(0.025)
      .velocityDecay(0.38)
      .stop();
    let topology = -1;

    function syncTopology() {
      if (topology === graphModel.topology) return;
      const first = topology === -1;
      topology = graphModel.topology;
      const nodes = [...graphModel.nodes.values()] as Drawn[];
      assignRings(nodes.filter((n) => !n.leaving));
      const ids = new Set(nodes.map((n) => n.id));
      const links: Link[] = graphModel.edges
        .filter((e) => e.kind === "similar" && ids.has(e.source) && ids.has(e.target))
        .map((e) => ({ source: e.source, target: e.target, similarity: e.similarity ?? 0.5, kind: e.kind }));
      sim.nodes(nodes);
      sim.force<ReturnType<typeof forceLink<GNode, Link>>>("link")!.links(links);
      sim.alpha(first ? 1 : Math.max(sim.alpha(), 0.35));
      if (first || reducedMotion()) for (let i = 0; i < 120; i++) sim.tick();
    }

    function resize() {
      dpr = Math.min(window.devicePixelRatio || 1, 2);
      width = container.clientWidth;
      height = container.clientHeight;
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
    }
    resize();
    const resizeObserver = new ResizeObserver(resize);
    resizeObserver.observe(container);

    // Zoom & pan. Graph coordinates are centered on (0, 0).
    const zb = zoom<HTMLCanvasElement, unknown>()
      .scaleExtent([0.25, 5])
      .on("zoom", (e: { transform: ZoomTransform }) => {
        transform.current = e.transform;
      });
    zoomBehavior.current = zb;
    const canvasSel = select(canvas);
    canvasSel.call(zb).on("dblclick.zoom", null);
    canvasSel.call(zb.transform, zoomIdentity.translate(width / 2, height / 2).scale(0.85));

    const themeObserver = new MutationObserver(() => (palette = readPalette()));
    themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["data-theme"] });
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const onScheme = () => (palette = readPalette());
    media.addEventListener("change", onScheme);

    function neighborsOf(id: string | null): Set<string> | null {
      if (!id || !graphModel.nodes.has(id)) return null;
      const set = new Set([id]);
      for (const e of graphModel.edges) {
        if (e.source === id) set.add(e.target);
        if (e.target === id) set.add(e.source);
      }
      return set;
    }

    let frame = 0;
    const draw = (now: number) => {
      const motion = !reducedMotion();
      graphModel.prune(now);
      syncTopology();
      if (sim.alpha() > sim.alphaMin()) sim.tick();
      if (pendingFit.current && graphModel.nodes.size > 0 && sim.alpha() < 0.2) {
        const tf = fitTransform([...graphModel.nodes.values()], width, height);
        if (tf) {
          canvasSel.call(zb.transform, tf);
          pendingFit.current = false;
        }
      }

      const t = transform.current;
      const k = t.k;
      const v = viewRef.current;
      const reference = v.mode === "at" && v.time ? Date.parse(v.time) : Date.now();
      const focus = neighborsOf(selectedRef.current);

      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
      ctx.fillStyle = palette.bg;
      ctx.fillRect(0, 0, width, height);
      ctx.save();
      ctx.translate(t.x, t.y);
      ctx.scale(k, k);

      // Similarity edges: hairlines, stronger for closer stories.
      for (const link of sim.force<ReturnType<typeof forceLink<GNode, Link>>>("link")!.links()) {
        const a = link.source as GNode;
        const b = link.target as GNode;
        if (a.x === undefined || b.x === undefined) continue;
        const inFocus = focus && focus.has(a.id) && focus.has(b.id);
        const strength = clamp01((link.similarity - 0.3) / 0.45);
        ctx.strokeStyle = inFocus ? palette.ink2 : palette.grid;
        ctx.globalAlpha = (focus && !inFocus ? 0.25 : 1) * (inFocus ? 0.9 : 0.35 + 0.65 * strength);
        ctx.lineWidth = (inFocus ? 1.6 : 1) / k;
        ctx.beginPath();
        ctx.moveTo(a.x, a.y!);
        ctx.lineTo(b.x, b.y!);
        ctx.stroke();
      }
      ctx.globalAlpha = 1;

      // Lineage flashes: dashed violet arcs between parent/child or merged stories.
      for (const f of graphModel.flashes) {
        const a = graphModel.nodes.get(f.from);
        const b = graphModel.nodes.get(f.to);
        if (!a?.x || !b?.x) continue;
        const life = clamp01((now - f.at) / FLASH_MS);
        ctx.strokeStyle = withAlpha(palette.lineage, 0.85 * (1 - life));
        ctx.lineWidth = 2 / k;
        ctx.setLineDash([6 / k, 5 / k]);
        ctx.lineDashOffset = motion ? -now / 40 / k : 0;
        ctx.beginPath();
        ctx.moveTo(a.x, a.y!);
        ctx.lineTo(b.x, b.y!);
        ctx.stroke();
        ctx.setLineDash([]);
      }

      // Nodes: large first so small ones stay visible on top.
      const nodes = [...graphModel.nodes.values()] as Drawn[];
      nodes.sort((a, b) => radius(b.articleCount) - radius(a.articleCount));
      for (const n of nodes) {
        if (n.x === undefined || n.y === undefined) continue;
        const target = radius(n.articleCount);
        n.dr = n.dr === undefined || !motion ? target : n.dr + (target - n.dr) * 0.12;
        let r = n.dr;
        let alpha = focus ? (focus.has(n.id) ? 1 : 0.22) : (n.prominence ?? 1);
        let x = n.x;
        let y = n.y;

        if (motion && n.bornAt !== null) {
          const life = clamp01((now - n.bornAt) / BIRTH_MS);
          r *= life >= 1 ? 1 : Math.max(0.05, easeOutBack(life));
        }
        if (n.leaving) {
          const life = clamp01((now - n.leaving.at) / LEAVE_MS);
          const e = easeInOut(life);
          if (n.leaving.kind === "merged" && n.leaving.into) {
            const into = graphModel.nodes.get(n.leaving.into);
            if (into?.x !== undefined) {
              x = n.x + (into.x - n.x) * e;
              y = n.y + (into.y! - n.y) * e;
            }
            r *= 1 - 0.75 * e;
            alpha *= 1 - e * 0.6;
          } else if (n.leaving.kind === "split") {
            r *= 1 + 0.5 * e;
            alpha *= 1 - e;
          } else {
            r *= 1 - 0.35 * e;
            alpha *= 1 - e;
          }
        }

        const hours = (reference - n.lastActivity) / 3_600_000;
        ctx.globalAlpha = alpha;
        ctx.beginPath();
        ctx.arc(x, y, r, 0, Math.PI * 2);
        ctx.fillStyle = recencyColor(palette, hours);
        ctx.fill();
        // Surface ring separates overlapping marks.
        ctx.lineWidth = 1.5 / k;
        ctx.strokeStyle = palette.bg;
        ctx.stroke();

        if (n.id === selectedRef.current || n.id === hoveredRef.current) {
          ctx.beginPath();
          ctx.arc(x, y, r + 4 / k, 0, Math.PI * 2);
          ctx.lineWidth = (n.id === selectedRef.current ? 2.5 : 1.5) / k;
          ctx.strokeStyle = palette.ink1;
          ctx.stroke();
        }

        if (motion) {
          for (const p of n.pulses) {
            const life = clamp01((now - p.at) / PULSE_MS);
            const color = p.kind === "birth" ? palette.birth : p.kind === "lineage" ? palette.lineage : palette.article;
            ctx.beginPath();
            ctx.arc(x, y, r + (4 + 26 * easeInOut(life)) / Math.sqrt(k), 0, Math.PI * 2);
            ctx.lineWidth = (2.5 * (1 - life) + 0.5) / k;
            ctx.strokeStyle = withAlpha(color, 0.95 * (1 - life));
            ctx.stroke();
          }
        }
      }
      ctx.globalAlpha = 1;
      ctx.restore();

      // Labels in screen space: the biggest stories plus hover/selection, no overlaps.
      const live = graphModel.live().sort((a, b) => b.sourceCount - a.sourceCount || b.articleCount - a.articleCount);
      const want = new Set(live.slice(0, LABELS).map((n) => n.id));
      if (selectedRef.current) want.add(selectedRef.current);
      if (hoveredRef.current) want.add(hoveredRef.current);
      const placed: [number, number, number, number][] = [];
      const priority = [selectedRef.current, hoveredRef.current].filter(Boolean) as string[];
      const order = [...priority, ...live.map((n) => n.id).filter((id) => want.has(id) && !priority.includes(id))];
      ctx.font = "600 12px 'Inter Variable', system-ui, sans-serif";
      ctx.textAlign = "center";
      ctx.textBaseline = "top";
      for (const id of order) {
        const n = graphModel.nodes.get(id) as Drawn | undefined;
        if (!n || n.x === undefined || n.leaving) continue;
        const nx = n.x * k + t.x;
        const sy = n.y! * k + t.y + (n.dr ?? radius(n.articleCount)) * k + 6;
        if (nx < 0 || nx > width || sy < -20 || sy > height + 20) continue;
        const text = truncate(n.headline, width < 520 ? 28 : 38);
        const w = ctx.measureText(text).width;
        // Keep the label inside the canvas even when its node is near an edge.
        const sx = Math.max(w / 2 + 6, Math.min(width - w / 2 - 6, nx));
        const box: [number, number, number, number] = [sx - w / 2 - 4, sy - 2, w + 8, 18];
        const clash = placed.some(
          (b) => box[0] < b[0] + b[2] && box[0] + box[2] > b[0] && box[1] < b[1] + b[3] && box[1] + box[3] > b[1],
        );
        if (clash && !priority.includes(id)) continue;
        placed.push(box);
        ctx.globalAlpha = focus && !focus.has(id) ? 0.35 : 1;
        ctx.lineWidth = 4;
        ctx.lineJoin = "round";
        ctx.strokeStyle = palette.bg;
        ctx.strokeText(text, sx, sy);
        ctx.fillStyle = priority.includes(id) ? palette.ink1 : palette.ink2;
        ctx.fillText(text, sx, sy);
      }
      ctx.globalAlpha = 1;
      frame = requestAnimationFrame(draw);
    };
    frame = requestAnimationFrame(draw);

    // Hit testing in graph coordinates.
    function nodeAt(clientX: number, clientY: number): GNode | null {
      const rect = canvas.getBoundingClientRect();
      const [gx, gy] = transform.current.invert([clientX - rect.left, clientY - rect.top]);
      let best: GNode | null = null;
      let bestDist = Infinity;
      for (const n of graphModel.live()) {
        if (n.x === undefined) continue;
        const d = Math.hypot(n.x - gx, n.y! - gy);
        const r = radius(n.articleCount) + 3 / transform.current.k;
        if (d <= r && d < bestDist) {
          best = n;
          bestDist = d;
        }
      }
      return best;
    }
    const onMove = (e: PointerEvent) => {
      const n = nodeAt(e.clientX, e.clientY);
      hoveredRef.current = n?.id ?? null;
      canvas.style.cursor = n ? "pointer" : "grab";
      const rect = container.getBoundingClientRect();
      setHover(n ? { id: n.id, x: e.clientX - rect.left, y: e.clientY - rect.top } : null);
    };
    const onLeave = () => {
      hoveredRef.current = null;
      setHover(null);
    };
    const onClick = (e: MouseEvent) => {
      const n = nodeAt(e.clientX, e.clientY);
      useApp.getState().select(n?.id ?? null);
    };
    canvas.addEventListener("pointermove", onMove);
    canvas.addEventListener("pointerleave", onLeave);
    canvas.addEventListener("click", onClick);

    return () => {
      cancelAnimationFrame(frame);
      sim.stop();
      resizeObserver.disconnect();
      themeObserver.disconnect();
      media.removeEventListener("change", onScheme);
      canvas.removeEventListener("pointermove", onMove);
      canvas.removeEventListener("pointerleave", onLeave);
      canvas.removeEventListener("click", onClick);
    };
  }, []);

  // Bring the selected story into view with a short eased pan.
  useEffect(() => {
    const node = selected ? graphModel.nodes.get(selected) : undefined;
    const canvas = canvasRef.current;
    const zb = zoomBehavior.current;
    if (!node || node.x === undefined || !canvas || !zb) return;
    // The target is recomputed every frame: the drawer opening resizes the
    // canvas mid-tween, and the node itself may still be settling.
    const target = (k: number) => {
      // When the drawer overlays the graph (narrow layouts), center in the part left visible.
      const drawer = document.querySelector(".drawer");
      const overlay = drawer && getComputedStyle(drawer).position === "fixed" ? drawer.clientWidth : 0;
      // The container, not the canvas: the canvas's size lags until the ResizeObserver runs.
      const box = canvas.parentElement ?? canvas;
      const width = Math.max(200, box.clientWidth - overlay);
      return zoomIdentity.translate(width / 2 - node.x! * k, box.clientHeight / 2 - node.y! * k).scale(k);
    };
    const from = transform.current;
    const k = Math.max(from.k, 1.1);
    const start = performance.now();
    const duration = reducedMotion() ? 0 : 500;
    let frame = 0;
    const step = (now: number) => {
      const p = duration === 0 ? 1 : easeInOut(clamp01((now - start) / duration));
      const to = target(k);
      const tf = zoomIdentity
        .translate(from.x + (to.x - from.x) * p, from.y + (to.y - from.y) * p)
        .scale(from.k + (to.k - from.k) * p);
      select(canvas).call(zb.transform, tf);
      if (p < 1) frame = requestAnimationFrame(step);
    };
    frame = requestAnimationFrame(step);
    return () => cancelAnimationFrame(frame);
  }, [selected]);

  const zoomBy = (factor: number) => {
    const canvas = canvasRef.current;
    if (canvas && zoomBehavior.current) select(canvas).call(zoomBehavior.current.scaleBy, factor);
  };
  const fit = () => {
    const canvas = canvasRef.current;
    if (!canvas || !zoomBehavior.current) return;
    const tf = fitTransform([...graphModel.nodes.values()], canvas.clientWidth, canvas.clientHeight);
    if (tf) select(canvas).call(zoomBehavior.current.transform, tf);
  };

  return (
    <div className="graph" ref={containerRef}>
      <canvas
        ref={canvasRef}
        className="graph__canvas"
        role="img"
        aria-label={`Story graph: ${graphModel.live().length} stories. The story list beside it has the same stories in text form.`}
      />
      {hover && <GraphTooltip id={hover.id} x={hover.x} y={hover.y} />}
      {graph.isLoading && <div className="graph__state">Loading stories…</div>}
      {graph.isError && <div className="graph__state">Couldn't load the graph. Retrying…</div>}
      {!graph.isLoading && !graph.isError && empty && (
        <div className="graph__state">No multi-source stories yet. They appear as soon as two outlets cover the same event.</div>
      )}
      <div className="graph__zoom" role="group" aria-label="Zoom">
        <button className="icon-btn" onClick={() => zoomBy(1.3)} aria-label="Zoom in" title="Zoom in">
          <Plus size={16} />
        </button>
        <button className="icon-btn" onClick={() => zoomBy(1 / 1.3)} aria-label="Zoom out" title="Zoom out">
          <Minus size={16} />
        </button>
        <button className="icon-btn" onClick={fit} aria-label="Fit all stories" title="Fit all stories">
          <Maximize2 size={15} />
        </button>
      </div>
    </div>
  );
}
