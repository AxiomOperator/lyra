// Read-back: lyra's replies spoken by the device's own voice (the browser's
// speech synthesis: phones and desktops, no server). A speaker on each reply;
// "read replies aloud" (for the car) and the voice are kept on this device.

import { VoiceSelector, VoiceSelectorContent, VoiceSelectorEmpty, VoiceSelectorGroup, VoiceSelectorInput, VoiceSelectorItem, VoiceSelectorList, VoiceSelectorTrigger } from "@/components/ai-elements/voice-selector";
import { PromptInputButton } from "@/components/ai-elements/prompt-input";
import { cn } from "@/lib/utils";
import { Check, Square, Volume2, VolumeX } from "lucide-react";
import { useCallback, useEffect, useState } from "react";

export const canSpeak = typeof window !== "undefined" && "speechSynthesis" in window;

type Prefs = { auto: boolean; voice?: string; rate: number };

function load(): Prefs {
  try {
    return { auto: false, rate: 1, ...JSON.parse(localStorage.getItem("lyra-voice") ?? "{}") };
  } catch {
    return { auto: false, rate: 1 };
  }
}

const listeners = new Set<(p: Prefs) => void>();
let prefs: Prefs = load();

function setPrefs(p: Partial<Prefs>) {
  prefs = { ...prefs, ...p };
  try {
    localStorage.setItem("lyra-voice", JSON.stringify(prefs));
  } catch {
    // just not remembered
  }
  listeners.forEach((fn) => fn(prefs));
}

export function useVoicePrefs() {
  const [p, set] = useState(prefs);
  useEffect(() => {
    listeners.add(set);
    return () => void listeners.delete(set);
  }, []);
  return p;
}

/** Markdown as something to say: no code, links as their words, no symbols. */
export function speakable(md: string): string {
  return md
    .replace(/```[\s\S]*?```/g, " (code left out) ")
    .replace(/`([^`]+)`/g, "$1")
    .replace(/!\[[^\]]*\]\([^)]*\)/g, "")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/^\s{0,3}#{1,6}\s*/gm, "")
    .replace(/^\s*[-*+]\s+/gm, "")
    .replace(/^\s*\|.*\|\s*$/gm, (row) => row.replace(/\|/g, ", ").replace(/-{3,}/g, ""))
    .replace(/[*_~]{1,3}([^*_~]+)[*_~]{1,3}/g, "$1")
    .replace(/https?:\/\/\S+/g, "a link")
    .replace(/\n{2,}/g, ". ")
    .replace(/\s+/g, " ")
    .trim();
}

// What's being said now (one at a time), for the speaker buttons.
let speakingId: string | null = null;
const speakingListeners = new Set<(id: string | null) => void>();
function setSpeaking(id: string | null) {
  speakingId = id;
  speakingListeners.forEach((fn) => fn(id));
}

export function speak(id: string, text: string) {
  if (!canSpeak) return;
  const say = speakable(text);
  if (!say) return;
  speechSynthesis.cancel();
  const u = new SpeechSynthesisUtterance(say);
  const v = speechSynthesis.getVoices().find((v) => v.voiceURI === prefs.voice);
  if (v) {
    u.voice = v;
    u.lang = v.lang;
  }
  u.rate = prefs.rate;
  u.onend = u.onerror = () => speakingId === id && setSpeaking(null);
  setSpeaking(id);
  speechSynthesis.speak(u);
}

export function stopSpeaking() {
  if (canSpeak) speechSynthesis.cancel();
  setSpeaking(null);
}

/** The speaker under a reply: read it, or stop. */
export function SpeakButton({ id, text }: { id: string; text: string }) {
  const [now, setNow] = useState(speakingId);
  useEffect(() => {
    speakingListeners.add(setNow);
    return () => void speakingListeners.delete(setNow);
  }, []);
  if (!canSpeak || !text.trim()) return null;
  const on = now === id;
  return (
    <button type="button" aria-label={on ? "Stop reading" : "Read aloud"} title={on ? "Stop reading" : "Read aloud"} onClick={() => (on ? stopSpeaking() : speak(id, text))} className={cn("rounded p-0.5 hover:text-foreground", on && "text-primary")}>
      {on ? <Square className="size-3.5" /> : <Volume2 className="size-3.5" />}
    </button>
  );
}

/** The device's voices (they arrive a moment after the page loads). */
function useVoices() {
  const [voices, setVoices] = useState<SpeechSynthesisVoice[]>(() => (canSpeak ? speechSynthesis.getVoices() : []));
  useEffect(() => {
    if (!canSpeak) return;
    const load = () => setVoices(speechSynthesis.getVoices());
    load();
    speechSynthesis.addEventListener("voiceschanged", load);
    return () => speechSynthesis.removeEventListener("voiceschanged", load);
  }, []);
  return voices;
}

/** In the composer: read replies aloud on or off, and which voice. */
export function VoiceButton() {
  const p = useVoicePrefs();
  const voices = useVoices();
  const [open, setOpen] = useState(false);
  const pick = useCallback((uri?: string) => {
    setPrefs({ voice: uri });
    const v = speechSynthesis.getVoices().find((v) => v.voiceURI === uri);
    if (v) speak("sample", `Hi, I'm lyra. This is how I'll sound.`);
  }, []);
  if (!canSpeak) return null;
  // Your language's voices first.
  const lang = navigator.language.split("-")[0];
  const sorted = [...voices].sort((a, b) => Number(b.lang.startsWith(lang)) - Number(a.lang.startsWith(lang)) || a.name.localeCompare(b.name));
  return (
    <VoiceSelector open={open} onOpenChange={setOpen} value={p.voice} onValueChange={pick}>
      <VoiceSelectorTrigger asChild>
        <PromptInputButton aria-label="Read replies aloud" title={p.auto ? "Reading replies aloud" : "Read replies aloud"} className={cn(p.auto && "text-primary")}>
          {p.auto ? <Volume2 className="size-4" /> : <VolumeX className="size-4" />}
        </PromptInputButton>
      </VoiceSelectorTrigger>
      <VoiceSelectorContent title="Read replies aloud">
        <div className="flex items-center justify-between gap-3 border-b px-4 py-3">
          <div>
            <div className="font-medium text-sm">Read replies aloud</div>
            <div className="text-muted-foreground text-xs">Each reply is spoken when it's done, on this device (e.g. in the car).</div>
          </div>
          <button
            type="button"
            role="switch"
            aria-checked={p.auto}
            onClick={() => {
              setPrefs({ auto: !p.auto });
              if (p.auto) stopSpeaking();
            }}
            className={cn("relative h-6 w-11 shrink-0 rounded-full transition-colors", p.auto ? "bg-primary" : "bg-muted")}
          >
            <span className={cn("absolute top-0.5 size-5 rounded-full bg-background transition-all", p.auto ? "left-5.5" : "left-0.5")} />
          </button>
        </div>
        <div className="flex items-center gap-2 border-b px-4 py-2 text-xs">
          <span className="text-muted-foreground">Speed</span>
          {[0.9, 1, 1.15, 1.3].map((r) => (
            <button key={r} type="button" onClick={() => setPrefs({ rate: r })} className={cn("rounded px-2 py-0.5", p.rate === r ? "bg-primary/15 text-primary" : "hover:bg-muted")}>
              {r}×
            </button>
          ))}
        </div>
        <VoiceSelectorInput placeholder="Find a voice…" />
        <VoiceSelectorList className="max-h-72">
          <VoiceSelectorEmpty>No voices on this device.</VoiceSelectorEmpty>
          <VoiceSelectorGroup heading="This device's voices">
            {sorted.map((v) => (
              <VoiceSelectorItem key={v.voiceURI} value={`${v.name} ${v.lang}`} onSelect={() => pick(v.voiceURI)}>
                <span className="min-w-0 flex-1 truncate">{v.name}</span>
                <span className="shrink-0 text-muted-foreground text-xs">{v.lang}</span>
                {p.voice === v.voiceURI && <Check className="size-4 text-primary" />}
              </VoiceSelectorItem>
            ))}
          </VoiceSelectorGroup>
        </VoiceSelectorList>
      </VoiceSelectorContent>
    </VoiceSelector>
  );
}

/** Speaks each finished reply when "read replies aloud" is on. */
export function useAutoRead(lastReply: { id: string; text: string } | null, waiting: boolean) {
  const p = useVoicePrefs();
  const [spoken, setSpoken] = useState<string | null>(null);
  useEffect(() => {
    if (!p.auto || waiting || !lastReply || lastReply.id === spoken) return;
    setSpoken(lastReply.id);
    speak(lastReply.id, lastReply.text);
  }, [p.auto, waiting, lastReply, spoken]);
  // Don't read out the reply that was there when it was switched on.
  useEffect(() => {
    if (p.auto && lastReply) setSpoken(lastReply.id);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [p.auto]);
}
