// Hands-free: talk to lyra without touching the screen (the car, the desk).
// Listen → send what was said → wait for the reply → read it aloud → listen
// again. lyra asking for a yes is read out and answered by voice ("yes" /
// "no"). "Stop" (or "that's all") ends it; three silences in a row pause it
// (tap to go on). Everything happens on this device: the browser's own
// speech recognition and voice, no server. The screen is kept awake while
// it's on.

import { PromptInputButton } from "@/components/ai-elements/prompt-input";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { Mic, MicOff, Play, Square, Volume2 } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { useLyra } from "./store";
import type { Approval } from "./types";
import { canSpeak, speak, stopSpeaking } from "./voice";

type Recognition = {
  lang: string;
  interimResults: boolean;
  continuous: boolean;
  onresult: ((e: { results: ArrayLike<ArrayLike<{ transcript: string }> & { isFinal: boolean }> }) => void) | null;
  onend: (() => void) | null;
  onerror: ((e: { error: string }) => void) | null;
  start: () => void;
  abort: () => void;
};

function recognizer(): (new () => Recognition) | null {
  if (typeof window === "undefined") return null;
  const w = window as unknown as { SpeechRecognition?: new () => Recognition; webkitSpeechRecognition?: new () => Recognition };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition ?? null;
}

/** This device can listen and speak. */
export const canHandsFree = canSpeak && recognizer() !== null;

/** What a heard phrase means, beyond a message. */
export function meaning(said: string): "stop" | "yes" | "no" | null {
  const t = said.toLowerCase().replace(/[^a-z' ]/g, " ").replace(/\s+/g, " ").trim();
  if (/^(stop|stop listening|that's all|that is all|hands free off|goodbye|bye|end|quit|exit)( please| thanks| thank you)?$/.test(t)) return "stop";
  if (/^(yes|yeah|yep|yup|sure|ok|okay|allow|allow it|go ahead|do it|approve|approved|please do)( please| thanks| thank you)?$/.test(t)) return "yes";
  if (/^(no|nope|nah|deny|don't|do not|cancel|stop it|not now|decline)( please| thanks| thank you)?$/.test(t)) return "no";
  return null;
}

type State = "off" | "listening" | "sending" | "waiting" | "speaking" | "approval" | "paused";

const WORDS: Record<State, string> = {
  off: "",
  listening: "Listening…",
  sending: "Sending…",
  waiting: "lyra is working on it…",
  speaking: "Reading the reply…",
  approval: "lyra is asking for your yes: say yes or no",
  paused: "Paused: tap to go on",
};

// Shared with the composer's button (one hands-free at a time).
let current: State = "off";
const watchers = new Set<(s: State) => void>();
function setShared(s: State) {
  current = s;
  watchers.forEach((fn) => fn(s));
}
let wanted = false;
const wantWatchers = new Set<(on: boolean) => void>();
function setWanted(on: boolean) {
  wanted = on;
  wantWatchers.forEach((fn) => fn(on));
}

export function useHandsFreeOn() {
  const [on, setOn] = useState(wanted);
  useEffect(() => {
    wantWatchers.add(setOn);
    return () => void wantWatchers.delete(setOn);
  }, []);
  return on;
}

/** The composer's switch. */
export function HandsFreeButton() {
  const on = useHandsFreeOn();
  const { connected } = useLyra();
  if (!canHandsFree) return null;
  return (
    <PromptInputButton
      aria-label="Hands-free"
      aria-pressed={on}
      title={on ? "Hands-free is on: say \"stop\" or tap to end" : "Hands-free: talk to lyra and hear its answers, no screen needed"}
      disabled={!connected}
      onClick={() => setWanted(!on)}
      className={cn(on && "bg-emerald-900/50 text-emerald-200 hover:bg-emerald-900/70")}
    >
      {on ? <Mic className="size-4" /> : <MicOff className="size-4" />}
      {on && <span className="text-xs">Hands-free</span>}
    </PromptInputButton>
  );
}

/**
 * The loop, while hands-free is on: listens, sends, waits for the reply,
 * reads it, listens again; reads out lyra's questions and takes a spoken
 * yes or no. Shown as a bar above the composer.
 */
export function HandsFreeBar({ lastReply, waiting, approvals }: { lastReply: { id: string; text: string } | null; waiting: boolean; approvals: Approval[] }) {
  const { say, send } = useLyra();
  const on = useHandsFreeOn();
  const [state, setState] = useState<State>("off");
  const [heard, setHeard] = useState("");
  const rec = useRef<Recognition | null>(null);
  const run = useRef(0); // each start of the loop; a stale callback sees it changed
  const misses = useRef(0);
  const sentAfter = useRef<string | null>(null); // the reply showing when we sent
  const asked = useRef<number | null>(null); // the approval read out
  const lock = useRef<{ release: () => Promise<void> } | null>(null);
  const lastReplyRef = useRef(lastReply);
  lastReplyRef.current = lastReply;

  const go = useCallback((s: State) => {
    setState(s);
    setShared(s);
  }, []);

  const quiet = useCallback(() => {
    try {
      rec.current?.abort();
    } catch {
      // already stopped
    }
    rec.current = null;
  }, []);

  // Listen once; `then` gets what was said (empty: nothing).
  const listen = useCallback(
    (then: (said: string) => void) => {
      const Rec = recognizer();
      if (!Rec) return;
      quiet();
      const mine = run.current;
      const r = new Rec();
      r.lang = navigator.language || "en-US";
      r.interimResults = true;
      r.continuous = false;
      let said = "";
      r.onresult = (e) => {
        said = Array.from(e.results)
          .map((x) => x[0]?.transcript ?? "")
          .join(" ")
          .replace(/\s+/g, " ")
          .trim();
        setHeard(said);
      };
      r.onerror = () => {};
      r.onend = () => {
        if (run.current !== mine) return;
        rec.current = null;
        then(said);
      };
      rec.current = r;
      setHeard("");
      try {
        r.start();
      } catch {
        then("");
      }
    },
    [quiet],
  );

  const speakThen = useCallback(
    (id: string, text: string, then: () => void) => {
      const mine = run.current;
      go("speaking");
      speak(id, text, () => run.current === mine && then());
    },
    [go],
  );

  // What to do with what was heard.
  const heardRef = useRef<(said: string) => void>(() => {});
  const ear = useCallback(() => {
    go("listening");
    listen((said) => heardRef.current(said));
  }, [go, listen]);

  heardRef.current = (said: string) => {
    if (!wanted) return;
    const what = meaning(said);
    if (state === "approval" || asked.current !== null) {
      const a = approvals.find((x) => x.id === asked.current);
      if (a && (what === "yes" || what === "no")) {
        send({ type: "approve", id: a.id, answer: what === "yes" ? "y" : "n" });
        asked.current = null;
        speakThen("hf-ok", what === "yes" ? "Allowed." : "Not allowed.", () => go("waiting"));
        return;
      }
      if (what === "stop") return setWanted(false);
      speakThen("hf-again", "Say yes or no.", ear);
      return;
    }
    if (!said) {
      misses.current += 1;
      if (misses.current >= 3) {
        go("paused");
        speak("hf-paused", "Hands-free paused. Tap to go on.");
        return;
      }
      return ear();
    }
    misses.current = 0;
    if (what === "stop") return setWanted(false);
    sentAfter.current = lastReplyRef.current?.id ?? null;
    go("sending");
    if (say(said)) go("waiting");
    else speakThen("hf-fail", "I couldn't send that: lyra isn't connected.", ear);
  };

  // Turning it on and off.
  useEffect(() => {
    if (on) {
      run.current += 1;
      misses.current = 0;
      asked.current = null;
      stopSpeaking();
      // Keep the screen awake (a phone in the car).
      const nav = navigator as unknown as { wakeLock?: { request: (t: string) => Promise<{ release: () => Promise<void> }> } };
      nav.wakeLock?.request("screen").then((l) => (lock.current = l)).catch(() => {});
      ear();
    } else {
      run.current += 1;
      quiet();
      stopSpeaking();
      lock.current?.release().catch(() => {});
      lock.current = null;
      if (state !== "off") speak("hf-off", "Hands-free off.");
      go("off");
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [on]);

  // The reply came: read it, then listen again.
  useEffect(() => {
    if (!on || state !== "waiting" || waiting || !lastReply || lastReply.id === sentAfter.current) return;
    sentAfter.current = lastReply.id;
    speakThen(lastReply.id, lastReply.text, ear);
  }, [on, state, waiting, lastReply, speakThen, ear]);

  // Finished with nothing new to say (an error, a stop): say so and listen.
  useEffect(() => {
    if (!on || state !== "waiting" || waiting) return;
    const t = window.setTimeout(() => {
      if (state === "waiting" && !waiting && lastReplyRef.current?.id === sentAfter.current) speakThen("hf-none", "No answer came. See the screen.", ear);
    }, 4000);
    return () => window.clearTimeout(t);
  }, [on, state, waiting, speakThen, ear]);

  // lyra asks for a yes: read it out, listen for yes or no.
  useEffect(() => {
    const a = approvals[0];
    if (!on || !a || asked.current === a.id || state === "speaking" || state === "listening") return;
    asked.current = a.id;
    quiet();
    go("approval");
    // What it is, in words (an address or a command line isn't for reading out).
    const said = a.detail
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l && !/^https?:\/\//.test(l) && l.length < 160);
    speakThen(`hf-ask-${a.id}`, `${a.agent} wants to ${a.what}.${said ? ` ${said}.` : ""} Say yes or no.`, () => {
      go("approval");
      listen((said) => heardRef.current(said));
    });
  }, [on, approvals, state, go, listen, quiet, speakThen]);

  // Leaving the page: nothing keeps listening.
  useEffect(() => () => quiet(), [quiet]);

  if (!on && state === "off") return null;
  return (
    <div className={cn("mx-3 mb-2 flex items-center gap-3 rounded-lg border px-3 py-2 text-sm", state === "approval" ? "border-amber-600/70 bg-amber-950/30" : "border-emerald-700/60 bg-emerald-950/20")} role="status" aria-live="polite">
      {state === "speaking" ? <Volume2 className="size-4 shrink-0 text-emerald-300" /> : <Mic className={cn("size-4 shrink-0 text-emerald-300", state === "listening" && "animate-pulse")} />}
      <div className="min-w-0 flex-1">
        <div className="font-medium">Hands-free · {WORDS[state] || "starting…"}</div>
        {heard && (state === "listening" || state === "approval" || state === "sending") && <div className="truncate text-muted-foreground text-xs">“{heard}”</div>}
        {state === "listening" && !heard && <div className="text-muted-foreground text-xs">Say what you need, or “stop”.</div>}
      </div>
      {state === "paused" ? (
        <Button size="sm" variant="secondary" onClick={() => ((misses.current = 0), ear())}>
          <Play /> Go on
        </Button>
      ) : null}
      <Button size="sm" variant="ghost" onClick={() => setWanted(false)} aria-label="End hands-free">
        <Square /> End
      </Button>
    </div>
  );
}

export function handsFreeState(): State {
  return current;
}
