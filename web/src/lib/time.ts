import { useSyncExternalStore } from "react";

import { useApp } from "../live/store";

// A shared clock that ticks every 30s, so "3m ago" labels advance without
// components reading Date.now() during render.
let now = Date.now();
const listeners = new Set<() => void>();
setInterval(() => {
  now = Date.now();
  listeners.forEach((l) => l());
}, 30_000);

function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

export function useNow(): number {
  return useSyncExternalStore(subscribe, () => now);
}

/** The time the view represents: the chosen past moment, or now when live. */
export function useReferenceTime(): number {
  const view = useApp((s) => s.view);
  const current = useNow();
  return view.mode === "at" && view.time ? Date.parse(view.time) : current;
}
