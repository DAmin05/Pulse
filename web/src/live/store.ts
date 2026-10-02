import { create } from "zustand";

import type { EventKind } from "../api/types";
import { GraphModel } from "../graph/model";

export type View = { mode: "live" } | { mode: "at"; offset: number; time: string | null };
export type Theme = "system" | "light" | "dark";
export type Connection = "connecting" | "live" | "reconnecting";

/** A recent live event, resolved to a headline for the activity ticker. */
export interface TickerItem {
  id: string;
  kind: EventKind;
  storyId: string;
  headline: string;
  detail?: string;
  at: number;
}

interface AppState {
  view: View;
  selected: string | null;
  lang: string | null;
  theme: Theme;
  metricsOpen: boolean;
  feedOpen: boolean;
  searchOpen: boolean;
  connection: Connection;
  /** Live events received while viewing the past. */
  missed: number;
  ticker: TickerItem[];
  /** Bumped when the graph model changes in a way overlays care about. */
  graphVersion: number;

  goLive: () => void;
  goTo: (offset: number, time: string | null) => void;
  select: (id: string | null) => void;
  setLang: (lang: string | null) => void;
  setTheme: (theme: Theme) => void;
  toggleMetrics: (open?: boolean) => void;
  toggleFeed: (open?: boolean) => void;
  setSearchOpen: (open: boolean) => void;
  setConnection: (c: Connection) => void;
  noteMissed: () => void;
  pushTicker: (item: TickerItem) => void;
  bumpGraph: () => void;
}

function storedTheme(): Theme {
  try {
    const t = localStorage.getItem("pulse.theme");
    return t === "light" || t === "dark" ? t : "system";
  } catch {
    return "system";
  }
}

export const useApp = create<AppState>((set) => ({
  view: { mode: "live" },
  selected: null,
  lang: null,
  theme: storedTheme(),
  metricsOpen: false,
  feedOpen: false,
  searchOpen: false,
  connection: "connecting",
  missed: 0,
  ticker: [],
  graphVersion: 0,

  goLive: () => set({ view: { mode: "live" }, missed: 0 }),
  goTo: (offset, time) => set({ view: { mode: "at", offset, time } }),
  select: (selected) => set({ selected }),
  setLang: (lang) => set({ lang }),
  setTheme: (theme) => {
    try {
      if (theme === "system") localStorage.removeItem("pulse.theme");
      else localStorage.setItem("pulse.theme", theme);
    } catch {
      /* storage unavailable: theme still applies for this session */
    }
    set({ theme });
  },
  toggleMetrics: (open) => set((s) => ({ metricsOpen: open ?? !s.metricsOpen })),
  toggleFeed: (open) => set((s) => ({ feedOpen: open ?? !s.feedOpen })),
  setSearchOpen: (searchOpen) => set({ searchOpen }),
  setConnection: (connection) => set({ connection }),
  noteMissed: () => set((s) => ({ missed: s.missed + 1 })),
  pushTicker: (item) => set((s) => ({ ticker: [item, ...s.ticker].slice(0, 40) })),
  bumpGraph: () => set((s) => ({ graphVersion: s.graphVersion + 1 })),
}));

/** The one graph model, shared by the stream (writer) and the canvas (reader). */
export const graphModel = new GraphModel({ minSources: 2, maxNodes: 160 });

/** Position parameter for API calls: undefined when live. */
export function viewAt(view: View): number | undefined {
  return view.mode === "at" ? view.offset : undefined;
}
