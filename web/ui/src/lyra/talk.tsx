// Talk: a conversation with lyra by voice alone. Tap the circle and talk;
// lyra hears when you've finished (your speech server, or the browser's),
// answers aloud (Kokoro, or the device's voice), and listens again. Talk over
// it to cut in, or tap to skip to listening. While this page is open lyra
// answers for the ear (short, plain sentences: no lists, tables or code).
// Approvals are read out and answered by a spoken yes or no, or the buttons.
// It's the same conversation as the chat: what's said here shows there too.

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { AudioLines, Loader2, Mic, Pause, Plus, Square, Volume2 } from "lucide-react";
import { useEffect, useMemo } from "react";
import { endHandsFree, HANDS_FREE_WORDS, setTalkOpen, startHandsFree, useCanHandsFree, useHandsFree, type HandsFreeState } from "./handsfree";
import { useServerSpeech } from "./speech";
import { useLyra } from "./store";
import { speakable, VoiceButton } from "./voice";

const LOOK: Record<HandsFreeState, { ring: string; core: string; label: string }> = {
  off: { ring: "border-muted", core: "bg-muted", label: "Tap to talk" },
  listening: { ring: "border-emerald-500/60 animate-pulse", core: "bg-emerald-500/20", label: "Listening" },
  sending: { ring: "border-sky-500/60", core: "bg-sky-500/20", label: "Got it" },
  waiting: { ring: "border-sky-500/60 animate-spin [animation-duration:3s] border-t-transparent", core: "bg-sky-500/15", label: "Thinking" },
  speaking: { ring: "border-violet-500/70 animate-pulse", core: "bg-violet-500/25", label: "Speaking" },
  approval: { ring: "border-amber-500/70 animate-pulse", core: "bg-amber-500/20", label: "Yes or no?" },
  paused: { ring: "border-muted", core: "bg-muted/60", label: "Paused" },
};

export function TalkPage() {
  const { messages, status, say, send, connected } = useLyra();
  const can = useCanHandsFree();
  const server = useServerSpeech();
  const approvals = useMemo(() => status.approvals ?? [], [status.approvals]);
  const lastReply = useMemo(() => {
    for (let i = messages.length - 1; i >= 0; i--) if (messages[i].role === "assistant" && messages[i].content.trim()) return { id: `${status.session ?? ""}-${i}`, text: messages[i].content };
    return null;
  }, [messages, status.session]);
  const hf = useHandsFree({ lastReply, waiting: !!status.waiting, approvals, active: true });

  // The page has the voice: the chat's hands-free bar stands aside, and lyra
  // answers for the ear until it's closed.
  useEffect(() => {
    setTalkOpen(true);
    return () => {
      endHandsFree();
      setTalkOpen(false);
    };
  }, []);
  useEffect(() => {
    if (!connected) return;
    say("/talk on");
    return () => void say("/talk off");
  }, [connected, say]);

  // The last exchange, as captions.
  const lastUser = [...messages].reverse().find((m) => m.role === "user")?.content.split("\n\n**Attached:")[0].trim() ?? "";
  const answer = lastReply ? speakable(lastReply.text) : "";
  const look = LOOK[hf.on ? hf.state : "off"];
  const tap = () => {
    if (!hf.on) return startHandsFree();
    if (hf.state === "paused") return hf.resume();
    // Speaking (or hearing nothing useful): straight to listening.
    if (hf.state === "speaking" || hf.state === "listening") return hf.skip();
  };
  const word = !hf.on ? (can ? "Tap the circle and talk" : "") : hf.state === "waiting" && status.phase ? status.phase.replace(/^↪\s*/, "") : HANDS_FREE_WORDS[hf.state];

  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex items-center justify-between gap-2 border-b px-4 py-2">
        <div className="flex items-center gap-2 font-medium">
          <AudioLines className="size-4 text-violet-400" /> Talk
          <span className="text-muted-foreground text-xs">{server?.stt || server?.tts ? "on lyra's speech servers" : "with this device's voice"}</span>
        </div>
        <div className="flex items-center gap-1">
          <VoiceButton />
          <Button size="sm" variant="ghost" onClick={() => say("/new")} title="Start a new conversation">
            <Plus /> New
          </Button>
        </div>
      </div>

      <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-8 overflow-y-auto px-4 py-6">
        {!can ? (
          <p className="max-w-md text-center text-muted-foreground text-sm">
            This browser can't listen and speak here. Use Chrome or Safari, or set up lyra's own speech servers (<span className="font-mono">[speech]</span>), which work in any browser that records.
          </p>
        ) : (
          <>
            <button
              type="button"
              onClick={tap}
              disabled={!connected}
              aria-label={hf.on ? `${look.label}: tap to ${hf.state === "speaking" ? "cut in" : hf.state === "paused" ? "go on" : "listen now"}` : "Start talking"}
              className="group relative flex size-56 items-center justify-center rounded-full outline-none sm:size-64"
            >
              <span className={cn("absolute inset-0 rounded-full border-4 transition-colors", look.ring)} />
              <span className={cn("absolute inset-6 rounded-full transition-colors", look.core)} />
              <span className="relative flex flex-col items-center gap-2 text-sm">
                {!hf.on ? <Mic className="size-10" /> : hf.state === "speaking" ? <Volume2 className="size-10 text-violet-300" /> : hf.state === "waiting" ? <Loader2 className="size-10 animate-spin text-sky-300" /> : hf.state === "paused" ? <Pause className="size-10" /> : <Mic className="size-10 text-emerald-300" />}
                <span className="font-medium">{look.label}</span>
              </span>
            </button>

            <div className="min-h-6 text-center text-muted-foreground text-sm" role="status" aria-live="polite">
              {word}
              {hf.on && hf.heard && (hf.state === "listening" || hf.state === "sending") && <div className="mt-1 text-foreground">“{hf.heard}”</div>}
            </div>

            {approvals[0] && (
              <div className="w-full max-w-lg rounded-lg border border-amber-600/60 bg-amber-950/20 px-4 py-3 text-sm">
                <div className="font-medium">
                  {approvals[0].agent} wants to {approvals[0].what}
                </div>
                <div className="mt-1 line-clamp-3 whitespace-pre-wrap text-muted-foreground text-xs">{approvals[0].detail}</div>
                <div className="mt-2 flex gap-2">
                  <Button size="sm" onClick={() => send({ type: "approve", id: approvals[0].id, answer: "y" })}>
                    Allow
                  </Button>
                  <Button size="sm" variant="secondary" onClick={() => send({ type: "approve", id: approvals[0].id, answer: "n" })}>
                    Deny
                  </Button>
                </div>
              </div>
            )}

            {(lastUser || answer) && (
              <div className="w-full max-w-2xl space-y-3 text-base">
                {lastUser && (
                  <p className="text-muted-foreground">
                    <span className="mr-2 text-xs uppercase tracking-wide">You</span>
                    {lastUser}
                  </p>
                )}
                {answer && !(hf.on && hf.state === "waiting") && (
                  <p className="leading-relaxed">
                    <span className="mr-2 text-violet-300 text-xs uppercase tracking-wide">lyra</span>
                    {answer.length > 700 ? `${answer.slice(0, 700)}…` : answer}
                  </p>
                )}
              </div>
            )}
          </>
        )}
      </div>

      {hf.on && (
        <div className="flex justify-center gap-3 border-t px-4 py-3">
          {hf.state === "paused" ? (
            <Button variant="secondary" onClick={hf.resume}>
              <Mic /> Go on
            </Button>
          ) : (
            <Button variant="secondary" onClick={hf.skip} title="Stop and listen now">
              <Mic /> Listen now
            </Button>
          )}
          <Button variant="ghost" onClick={endHandsFree} aria-label="End the conversation">
            <Square /> End
          </Button>
        </div>
      )}
    </div>
  );
}
