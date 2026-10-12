// Voice on lyra's own servers (`[speech]`: Whisper for speech to text,
// Kokoro for the voice). The app records and plays; lyra passes the audio to
// those servers (they stay inside the network). Without `[speech]` the
// browser's own recognition and voices are used, as before.
//
// Hands-free here hears when you stop talking itself (the microphone's level,
// with echo cancellation), and with interruptions on, talking over a reply
// being read stops it and listens.

import { useEffect, useState } from "react";

export type ServerSpeech = { stt: boolean; tts: { voice: string; format: string } | null; vad: boolean; interruptions: boolean; stream: boolean };

let current: ServerSpeech | null = null;
let token = "";
const watchers = new Set<(s: ServerSpeech | null) => void>();

/** Ask lyra what it can do (on connecting). */
export async function loadServerSpeech(t: string) {
  token = t;
  try {
    const r = await fetch("/api/speech", { headers: { Authorization: `Bearer ${t}` } });
    current = r.ok ? ((await r.json()) as ServerSpeech) : null;
  } catch {
    current = null;
  }
  if (current && !current.stt && !current.tts) current = null;
  watchers.forEach((fn) => fn(current));
}

export const serverSpeech = () => current;

export function useServerSpeech() {
  const [s, set] = useState(current);
  useEffect(() => {
    watchers.add(set);
    return () => void watchers.delete(set);
  }, []);
  return s;
}

export const canRecord = typeof window !== "undefined" && "MediaRecorder" in window && !!navigator.mediaDevices?.getUserMedia;

/** A recording, as text (lyra's speech-to-text server). */
export async function transcribe(blob: Blob): Promise<string> {
  const r = await fetch("/api/speech/transcribe", { method: "POST", headers: { Authorization: `Bearer ${token}`, "Content-Type": blob.type || "audio/webm" }, body: blob });
  const v = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(v.error || r.statusText);
  return String(v.text ?? "").trim();
}

/** What a reply is said in: sentences, a few at a time (so the first is heard at once). */
export function pieces(text: string, max = 240): string[] {
  const sentences = text.match(/[^.!?…]+[.!?…]+["')\]]*\s*|[^.!?…]+$/g) ?? [text];
  const out: string[] = [];
  let now = "";
  for (const s of sentences) {
    // The first piece short: it's what starts the voice.
    const limit = out.length === 0 ? 120 : max;
    if (now && (now + s).length > limit) {
      out.push(now.trim());
      now = "";
    }
    now += s;
  }
  if (now.trim()) out.push(now.trim());
  return out.filter(Boolean);
}

// The reply being said now: stopped by stopServerSay.
let playing: { stop: () => void } | null = null;

export function stopServerSay() {
  playing?.stop();
  playing = null;
}

/**
 * Say `text` with lyra's voice. With streaming, a sentence or two at a time:
 * the next is fetched while one plays. `done` once it's all said (not when
 * it's stopped). `rate` is the playback speed.
 */
export function serverSay(text: string, opts: { voice?: string; rate?: number; done?: () => void }) {
  stopServerSay();
  const abort = new AbortController();
  let audio: HTMLAudioElement | null = null;
  let stopped = false;
  const urls: string[] = [];
  playing = {
    stop: () => {
      stopped = true;
      abort.abort();
      audio?.pause();
      urls.forEach((u) => URL.revokeObjectURL(u));
    },
  };
  const all = current?.stream ? pieces(text) : [text];
  const fetchPiece = async (t: string) => {
    const r = await fetch("/api/speech/say", { method: "POST", signal: abort.signal, headers: { Authorization: `Bearer ${token}`, "Content-Type": "application/json" }, body: JSON.stringify({ text: t, voice: opts.voice }) });
    if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || r.statusText);
    const url = URL.createObjectURL(await r.blob());
    urls.push(url);
    return url;
  };
  (async () => {
    let next = all.length ? fetchPiece(all[0]) : null;
    for (let i = 0; i < all.length && next && !stopped; i++) {
      const url = await next;
      next = i + 1 < all.length ? fetchPiece(all[i + 1]) : null;
      next?.catch(() => {});
      if (stopped) return;
      await new Promise<void>((resolve) => {
        const a = new Audio(url);
        audio = a;
        a.playbackRate = opts.rate ?? 1;
        // Stuck (no sound output, the speaker taken by something else): on after 5 s.
        let last = -1;
        let still = 0;
        const watch = setInterval(() => {
          if (stopped) return finish();
          still = a.currentTime === last ? still + 1 : 0;
          last = a.currentTime;
          if (still >= 5) finish();
        }, 1000);
        function finish() {
          clearInterval(watch);
          resolve();
        }
        a.onended = a.onerror = finish;
        a.play().catch(finish);
      });
    }
    if (!stopped) opts.done?.();
  })().catch(() => {
    if (!stopped) opts.done?.();
  });
}

/** The voices lyra's speech server has. */
export async function serverVoices(): Promise<string[]> {
  try {
    const r = await fetch("/api/speech/voices", { headers: { Authorization: `Bearer ${token}` } });
    return r.ok ? ((await r.json()).voices as string[]) : [];
  } catch {
    return [];
  }
}

// ---- hearing when you talk (voice activity, from the microphone's level)

type Level = { stream: MediaStream; ctx: AudioContext; level: () => number; close: () => void };

async function microphone(): Promise<Level> {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true } });
  const ctx = new AudioContext();
  const src = ctx.createMediaStreamSource(stream);
  const an = ctx.createAnalyser();
  an.fftSize = 1024;
  src.connect(an);
  const buf = new Float32Array(an.fftSize);
  return {
    stream,
    ctx,
    level: () => {
      an.getFloatTimeDomainData(buf);
      let sum = 0;
      for (const x of buf) sum += x * x;
      return Math.sqrt(sum / buf.length);
    },
    close: () => {
      stream.getTracks().forEach((t) => t.stop());
      void ctx.close();
    },
  };
}

/** Loud enough to be speech (over a quiet room's hum). */
const SPEAKING = 0.035;
/** This long quiet after speech: they've finished. */
const DONE_AFTER = 1200;
/** Nothing said this long: a silence. */
const NOTHING_AFTER = 8000;

/**
 * Listen once: record from when they start talking until they stop (or a
 * silence), then the words (lyra's speech-to-text). `heard` shows progress.
 * The returned `stop` ends it at once (resolving with nothing).
 */
export function listenOnce(heard?: (what: "listening" | "hearing" | "thinking") => void): { done: Promise<string>; stop: () => void } {
  let stop = () => {};
  const done = (async () => {
    const mic = await microphone();
    const rec = new MediaRecorder(mic.stream);
    const chunks: Blob[] = [];
    rec.ondataavailable = (e) => e.data.size > 0 && chunks.push(e.data);
    let ended = false;
    const finished = new Promise<boolean>((resolve) => {
      const started = Date.now();
      let talking = false;
      let quietSince = Date.now();
      const tick = setInterval(() => {
        const l = mic.level();
        if (l > SPEAKING) {
          if (!talking) heard?.("hearing");
          talking = true;
          quietSince = Date.now();
        }
        const quiet = Date.now() - quietSince;
        if ((talking && quiet > DONE_AFTER) || (!talking && Date.now() - started > NOTHING_AFTER) || ended) {
          clearInterval(tick);
          resolve(talking && !ended);
        }
      }, 50);
      stop = () => {
        ended = true;
      };
    });
    heard?.("listening");
    rec.start(250);
    const said = await finished;
    const stopped = new Promise((r) => (rec.onstop = r));
    rec.stop();
    await stopped;
    mic.close();
    if (!said) return "";
    heard?.("thinking");
    return transcribe(new Blob(chunks, { type: rec.mimeType || "audio/webm" }));
  })();
  return { done, stop: () => stop() };
}

/**
 * While a reply is read: call `interrupted` once they start talking over it
 * (a moment of speech, louder than the echo the voice leaves). Returns `stop`.
 */
export function watchForInterruption(interrupted: () => void): () => void {
  let off = false;
  let close = () => {};
  void microphone()
    .then((mic) => {
      close = mic.close;
      if (off) return mic.close();
      let loud = 0;
      const tick = setInterval(() => {
        if (off) return clearInterval(tick);
        loud = mic.level() > SPEAKING * 2 ? loud + 1 : 0;
        // About a third of a second of talking.
        if (loud >= 7) {
          clearInterval(tick);
          off = true;
          mic.close();
          interrupted();
        }
      }, 50);
    })
    .catch(() => {});
  return () => {
    off = true;
    close();
  };
}
