import { describe, expect, it } from "vitest";

import type { BriefingSegment } from "../api/types";
import { clock, layout, pickVoice, timeOf, wordAt } from "./timing";

const seg = (text: string, kind: BriefingSegment["kind"] = "report"): BriefingSegment => ({
  kind,
  text,
  original_lang: "en",
  translated: false,
  source: null,
});

describe("layout", () => {
  it("assigns offsets matching the joined text", () => {
    const segments = [seg("Ship sinks.", "headline"), seg("BBC: A ship 😀 sank.")];
    const text = segments.map((s) => s.text).join(" ");
    const items = layout(segments);
    for (const item of items) {
      expect(text.slice(item.start, item.end)).toBe(item.segment.text);
      for (const t of item.tokens) expect(text.slice(t.offset, t.offset + t.text.length)).toBe(t.text);
    }
    const sank = items[1]!.tokens.find((t) => t.text === "sank.")!;
    // The emoji is two UTF-16 units, like the server's offsets.
    expect(sank.offset).toBe(text.indexOf("sank."));
  });
});

describe("wordAt", () => {
  const words: [number, number][] = [
    [0, 0],
    [5, 0.4],
    [11, 0.9],
  ];
  it("finds the word being spoken", () => {
    expect(wordAt(words, -1)).toBe(-1);
    expect(wordAt(words, 0)).toBe(0);
    expect(wordAt(words, 0.5)).toBe(1);
    expect(wordAt(words, 5)).toBe(2);
    expect(wordAt([], 1)).toBe(-1);
  });
  it("maps offsets back to times", () => {
    expect(timeOf(words, 11)).toBe(0.9);
    expect(timeOf(words, 3)).toBeNull();
  });
});

describe("clock", () => {
  it("formats minutes and seconds", () => {
    expect(clock(7.9)).toBe("0:07");
    expect(clock(83)).toBe("1:23");
  });
});

describe("pickVoice", () => {
  const voice = (name: string, lang: string, localService = true) =>
    ({ name, lang, localService, default: false, voiceURI: name }) as SpeechSynthesisVoice;
  it("prefers quality voices for the exact region", () => {
    const voices = [voice("Jorge", "es-ES"), voice("Paulina", "es-MX"), voice("Mónica (Enhanced)", "es-ES"), voice("Anna", "de-DE")];
    expect(pickVoice(voices, "es-ES")?.name).toBe("Mónica (Enhanced)");
    expect(pickVoice(voices, "es-MX")?.name).toBe("Mónica (Enhanced)");
    expect(pickVoice(voices, "ja-JP")).toBeNull();
  });
});
