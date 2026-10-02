// Transcript ↔ playback alignment. Offsets are UTF-16 indexes into the
// briefing text (JavaScript string indexes), as the API reports them.

import type { BriefingSegment } from "../api/types";

export interface Token {
  /** Offset of the token in the full briefing text. */
  offset: number;
  text: string;
  space: boolean;
}

export interface SegmentLayout {
  segment: BriefingSegment;
  start: number;
  end: number;
  tokens: Token[];
}

/** Segments are joined with single spaces to form the briefing text. */
export function layout(segments: BriefingSegment[]): SegmentLayout[] {
  let start = 0;
  return segments.map((segment) => {
    const tokens: Token[] = [];
    let offset = start;
    for (const part of segment.text.split(/(\s+)/)) {
      if (part) tokens.push({ offset, text: part, space: /^\s+$/.test(part) });
      offset += part.length;
    }
    const item = { segment, start, end: start + segment.text.length, tokens };
    start = item.end + 1;
    return item;
  });
}

/** Index of the last word starting at or before `seconds` (-1 before the first). */
export function wordAt(words: [number, number][], seconds: number): number {
  let lo = 0;
  let hi = words.length - 1;
  let found = -1;
  while (lo <= hi) {
    const mid = (lo + hi) >> 1;
    if (words[mid]![1] <= seconds) {
      found = mid;
      lo = mid + 1;
    } else {
      hi = mid - 1;
    }
  }
  return found;
}

/** Start time of the word at `offset`, if timings cover it. */
export function timeOf(words: [number, number][], offset: number): number | null {
  const w = words.find(([o]) => o === offset);
  return w ? w[1] : null;
}

/** "0:07", "1:23". */
export function clock(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, "0")}`;
}

/**
 * The best installed browser voice for a language: an exact regional match,
 * then any voice for the language, preferring higher-quality local voices.
 */
export function pickVoice(voices: SpeechSynthesisVoice[], bcp47: string): SpeechSynthesisVoice | null {
  const base = bcp47.split("-")[0]!.toLowerCase();
  const quality = (v: SpeechSynthesisVoice) =>
    (/(natural|neural|premium|enhanced|siri)/i.test(v.name) ? 4 : 0) +
    (v.lang.toLowerCase() === bcp47.toLowerCase() ? 2 : 0) +
    (v.localService ? 1 : 0);
  const matching = voices.filter((v) => v.lang.toLowerCase().replace("_", "-").split("-")[0] === base);
  return matching.sort((a, b) => quality(b) - quality(a))[0] ?? null;
}
