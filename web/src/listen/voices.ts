// The device's speech synthesis voices. Browsers load them asynchronously and
// announce changes with `voiceschanged`, so this re-renders when they arrive.

import { useSyncExternalStore } from "react";

const supported = typeof window !== "undefined" && "speechSynthesis" in window;
const EMPTY: SpeechSynthesisVoice[] = [];
let cached: SpeechSynthesisVoice[] = EMPTY;

function snapshot(): SpeechSynthesisVoice[] {
  if (!supported) return EMPTY;
  const voices = window.speechSynthesis.getVoices();
  // Keep the same array while nothing changed, as useSyncExternalStore requires.
  if (voices.length !== cached.length || voices.some((v, i) => v !== cached[i])) cached = voices;
  return cached;
}

function subscribe(onChange: () => void): () => void {
  if (!supported) return () => {};
  window.speechSynthesis.addEventListener("voiceschanged", onChange);
  return () => window.speechSynthesis.removeEventListener("voiceschanged", onChange);
}

export function useBrowserVoices(): SpeechSynthesisVoice[] {
  return useSyncExternalStore(subscribe, snapshot, () => EMPTY);
}
