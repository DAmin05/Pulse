// Plays a briefing: the synthesized audio when there is some, otherwise the
// browser's own speech synthesis, behind one interface. Either way it reports
// the offset of the word being spoken, for the transcript.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import type { Briefing } from "../api/types";
import { layout, pickVoice, wordAt } from "./timing";

export type Engine = "audio" | "browser";
export type Phase = "loading" | "ready" | "playing" | "paused" | "ended" | "error";

export interface Narration {
  engine: Engine | null;
  phase: Phase;
  /** Seconds (audio) or characters (browser) into the briefing. */
  position: number;
  /** Seconds (audio), characters (browser). */
  duration: number;
  /** UTF-16 offset of the word being spoken. */
  activeOffset: number | null;
  error: string | null;
  play: () => void;
  pause: () => void;
  /** Continue from the word at `offset`. */
  seekToOffset: (offset: number) => void;
  seekToFraction: (fraction: number) => void;
}

export const speechSupported = typeof window !== "undefined" && "speechSynthesis" in window;

export function useNarration(briefing: Briefing | undefined, autoplay: boolean, title: string): Narration {
  const engine: Engine | null = !briefing ? null : briefing.audio ? "audio" : speechSupported ? "browser" : null;
  const [phase, setPhase] = useState<Phase>("loading");
  const [time, setTime] = useState(0);
  const [spoken, setSpoken] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const autoplayRef = useRef(autoplay);
  // Browser speech: callbacks from cancelled utterances are ignored by generation.
  const generation = useRef(0);
  const items = useMemo(() => (briefing ? layout(briefing.segments) : []), [briefing]);

  useEffect(() => {
    autoplayRef.current = autoplay;
  }, [autoplay]);

  // --- Audio engine ---------------------------------------------------------
  const audioUrl = briefing?.audio?.url;
  useEffect(() => {
    if (!audioUrl) return;
    let cancelled = false;
    let objectUrl: string | null = null;
    const audio = new Audio();
    audio.preload = "auto";
    audioRef.current = audio;
    audio.onplay = () => setPhase("playing");
    audio.onpause = () => setPhase((p) => (p === "ended" ? p : "paused"));
    audio.onended = () => setPhase("ended");
    audio.onerror = () => {
      if (!cancelled && objectUrl) {
        setPhase("error");
        setError("The audio couldn't be played.");
      }
    };
    // Fetched whole, so seeking works without range requests.
    fetch(audioUrl)
      .then((r) => {
        if (!r.ok) throw new Error(`audio: ${r.status}`);
        return r.blob();
      })
      .then((blob) => {
        if (cancelled) return;
        objectUrl = URL.createObjectURL(blob);
        audio.src = objectUrl;
        setPhase("ready");
        if (autoplayRef.current) audio.play().catch(() => setPhase("paused"));
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        setPhase("error");
        setError(e instanceof Error ? e.message : "The audio couldn't be loaded.");
      });
    return () => {
      cancelled = true;
      audio.pause();
      audio.removeAttribute("src");
      if (objectUrl) URL.revokeObjectURL(objectUrl);
      audioRef.current = null;
    };
  }, [audioUrl]);

  // Playback clock, frame by frame while playing.
  useEffect(() => {
    if (engine !== "audio" || phase !== "playing") return;
    let frame = 0;
    const tick = () => {
      const audio = audioRef.current;
      if (audio) setTime(audio.currentTime);
      frame = requestAnimationFrame(tick);
    };
    frame = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(frame);
  }, [engine, phase]);

  // Lock screen / media keys.
  useEffect(() => {
    if (engine !== "audio" || !("mediaSession" in navigator) || typeof MediaMetadata === "undefined") return;
    navigator.mediaSession.metadata = new MediaMetadata({ title, artist: "Pulse briefing" });
    navigator.mediaSession.setActionHandler("play", () => void audioRef.current?.play());
    navigator.mediaSession.setActionHandler("pause", () => audioRef.current?.pause());
    return () => {
      navigator.mediaSession.metadata = null;
      navigator.mediaSession.setActionHandler("play", null);
      navigator.mediaSession.setActionHandler("pause", null);
    };
  }, [engine, title]);

  // --- Browser engine ---------------------------------------------------------
  const speakFrom = useCallback(
    (offset: number) => {
      if (!briefing || !speechSupported) return;
      const synth = window.speechSynthesis;
      const gen = ++generation.current;
      synth.cancel();
      const voice = pickVoice(synth.getVoices(), briefing.bcp47);
      // One utterance per segment: long utterances get cut off in some browsers.
      const queue = items.filter((it) => it.end > offset);
      queue.forEach((it, i) => {
        const from = Math.max(it.start, offset);
        const u = new SpeechSynthesisUtterance(it.segment.text.slice(from - it.start));
        u.lang = briefing.bcp47;
        if (voice) u.voice = voice;
        u.onstart = () => {
          if (gen !== generation.current) return;
          setPhase("playing");
          setSpoken(from);
        };
        u.onboundary = (e) => {
          if (gen === generation.current && e.name !== "sentence") setSpoken(from + e.charIndex);
        };
        u.onend = () => {
          if (gen === generation.current && i === queue.length - 1) setPhase("ended");
        };
        u.onerror = (e) => {
          if (gen !== generation.current || e.error === "interrupted" || e.error === "canceled") return;
          setPhase("error");
          setError(`Your browser couldn't read this (${e.error}).`);
        };
        synth.speak(u);
      });
    },
    [briefing, items],
  );

  // Start reading as soon as a browser-voice briefing arrives, if asked to.
  const browserText = engine === "browser" ? briefing?.text : undefined;
  const silence = useCallback(() => {
    generation.current++; // orphan the cancelled utterances' callbacks
    window.speechSynthesis.cancel();
  }, []);
  useEffect(() => {
    if (!browserText) return;
    if (autoplayRef.current) speakFrom(0);
    return silence;
  }, [browserText, speakFrom, silence]);

  // --- Interface ---------------------------------------------------------------
  const duration = engine === "audio" ? (briefing?.audio?.duration ?? 0) : (briefing?.text.length ?? 0);
  const words = briefing?.audio?.words;
  const audioActive = engine === "audio" && words && phase !== "loading" ? (words[wordAt(words, time)]?.[0] ?? null) : null;
  const activeOffset = engine === "audio" ? audioActive : spoken;
  const effectivePhase: Phase = engine === "browser" && phase === "loading" ? "ready" : phase;

  const play = useCallback(() => {
    if (engine === "audio") {
      const audio = audioRef.current;
      if (!audio) return;
      if (audio.ended) audio.currentTime = 0;
      audio.play().catch(() => setPhase("paused"));
    } else if (engine === "browser") {
      const synth = window.speechSynthesis;
      if (phase === "paused" && synth.paused) {
        synth.resume();
        setPhase("playing");
      } else {
        speakFrom(phase === "ended" ? 0 : (spoken ?? 0));
      }
    }
  }, [engine, phase, speakFrom, spoken]);

  const pause = useCallback(() => {
    if (engine === "audio") audioRef.current?.pause();
    else if (engine === "browser") {
      window.speechSynthesis.pause();
      setPhase("paused");
    }
  }, [engine]);

  const seekToOffset = useCallback(
    (offset: number) => {
      if (engine === "audio" && words && audioRef.current) {
        const w = words.find(([o]) => o >= offset);
        audioRef.current.currentTime = w ? w[1] : 0;
        setTime(audioRef.current.currentTime);
        if (audioRef.current.paused) audioRef.current.play().catch(() => setPhase("paused"));
      } else if (engine === "browser") {
        setSpoken(offset);
        speakFrom(offset);
      }
    },
    [engine, words, speakFrom],
  );

  const seekToFraction = useCallback(
    (fraction: number) => {
      const f = Math.min(1, Math.max(0, fraction));
      if (engine === "audio" && audioRef.current) {
        audioRef.current.currentTime = f * duration;
        setTime(audioRef.current.currentTime);
      } else if (engine === "browser" && briefing) {
        // Snap to the start of a word.
        const target = f * briefing.text.length;
        const token = items.flatMap((it) => it.tokens).find((t) => !t.space && t.offset >= target);
        seekToOffset(token?.offset ?? 0);
      }
    },
    [engine, duration, briefing, items, seekToOffset],
  );

  return {
    engine,
    phase: effectivePhase,
    position: engine === "audio" ? time : (spoken ?? 0),
    duration,
    activeOffset: effectivePhase === "ended" ? null : activeOffset,
    error,
    play,
    pause,
    seekToOffset,
    seekToFraction,
  };
}
