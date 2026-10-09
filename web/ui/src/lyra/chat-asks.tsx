// What lyra asks in the chat (a missing piece as a form, a round's steps
// before they run), failed tries folded into one line, and the chat's own
// controls: Chat only, edit your last message, try a reply again.

import { PromptInputButton } from "@/components/ai-elements/prompt-input";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { ChevronDown, ChevronRight, CircleHelp, ListChecks, MessagesSquare, Pencil, RefreshCw, RotateCcw, SkipForward, X } from "lucide-react";
import { useEffect, useState, type FormEvent, type ReactNode } from "react";
import { useLyra } from "./store";
import type { ChatAsk, ChatMessage } from "./types";

/** A missing piece of a call, as a form: filled in, the call goes on. */
function FillCard({ a }: { a: Extract<ChatAsk, { kind: "fill" }> }) {
  const { send } = useLyra();
  const [values, setValues] = useState<Record<string, string>>({});
  const [sent, setSent] = useState(false);
  const answer = (value: unknown) => {
    setSent(true);
    send({ type: "answer", id: a.id, value });
  };
  const submit = (e: FormEvent) => {
    e.preventDefault();
    answer(values);
  };
  return (
    <Card className="gap-2 border-2 border-sky-600/60 bg-sky-950/20 py-3">
      <CardContent className="space-y-3 px-4">
        <div className="flex items-start gap-2 text-sm">
          <CircleHelp className="mt-0.5 size-4 shrink-0 text-sky-400" />
          <div>
            <span className="font-medium">lyra needs a little more</span> for <span className="font-mono text-sky-300">{a.tool}</span>
            {a.what && <span className="text-muted-foreground"> ({a.what.toLowerCase()})</span>}
          </div>
        </div>
        <form onSubmit={submit} className="space-y-2">
          {a.fields.map((f, i) => (
            <label key={f.name} className="block space-y-1">
              <span className="text-muted-foreground text-xs">{f.label}</span>
              <Input
                autoFocus={i === 0}
                value={values[f.name] ?? ""}
                onChange={(e) => setValues((v) => ({ ...v, [f.name]: e.target.value }))}
                placeholder={f.hint || (f.list ? "With commas between" : "")}
                inputMode={f.name === "to" || f.name.includes("email") ? "email" : undefined}
                autoCapitalize={f.list ? "none" : undefined}
                aria-label={f.label}
              />
            </label>
          ))}
          <div className="flex gap-2">
            <Button type="submit" size="sm" disabled={sent || !Object.values(values).some((v) => v.trim())}>
              Go on
            </Button>
            <Button type="button" size="sm" variant="ghost" disabled={sent} onClick={() => answer("cancel")}>
              <X /> Not now
            </Button>
          </div>
        </form>
      </CardContent>
    </Card>
  );
}

/** Several steps at once: shown first, each one skippable; they start by themselves. */
function StepsCard({ a }: { a: Extract<ChatAsk, { kind: "steps" }> }) {
  const { send, say } = useLyra();
  const [skip, setSkip] = useState<string[]>([]);
  const [held, setHeld] = useState(false);
  const [left, setLeft] = useState(a.seconds);
  const [sent, setSent] = useState(false);
  useEffect(() => {
    if (held || sent) return;
    const t = window.setInterval(() => setLeft((s) => Math.max(0, s - 1)), 1000);
    return () => window.clearInterval(t);
  }, [held, sent]);
  const toggle = (id: string) => {
    // Choosing: lyra waits for "Go".
    if (!held) send({ type: "answer", id: a.id, value: "hold" });
    setHeld(true);
    setSkip((s) => (s.includes(id) ? s.filter((x) => x !== id) : [...s, id]));
  };
  const go = (value: unknown) => {
    setSent(true);
    send({ type: "answer", id: a.id, value });
  };
  return (
    <Card className="gap-2 border border-teal-700/60 bg-teal-950/15 py-3">
      <CardContent className="space-y-2 px-4">
        <div className="flex items-center gap-2 text-sm">
          <ListChecks className="size-4 shrink-0 text-teal-400" />
          <span className="font-medium">lyra is about to do {a.steps.length} things</span>
          <span className="ml-auto text-muted-foreground text-xs">{sent ? "going" : held ? "waiting for you" : `starts in ${left}s`}</span>
        </div>
        <ol className="space-y-1">
          {a.steps.map((s, i) => {
            const off = skip.includes(s.call_id);
            return (
              <li key={s.call_id} className="flex items-center gap-2 text-sm">
                <span className="w-4 shrink-0 text-right text-muted-foreground text-xs">{i + 1}</span>
                <span className={cn("min-w-0 flex-1 truncate", off && "text-muted-foreground line-through")}>
                  <span className="font-mono text-teal-300">{s.name}</span>
                  {s.summary && <span className="text-muted-foreground"> · {s.summary}</span>}
                </span>
                <Button size="sm" variant={off ? "secondary" : "ghost"} className="h-7 shrink-0 px-2 text-xs" disabled={sent} onClick={() => toggle(s.call_id)}>
                  <SkipForward className="size-3" /> {off ? "Skipped" : "Skip"}
                </Button>
              </li>
            );
          })}
        </ol>
        <div className="flex flex-wrap items-center gap-2">
          <Button size="sm" disabled={sent} onClick={() => go(skip.length ? { skip } : "go")}>
            {skip.length ? `Go without ${skip.length}` : "Go now"}
          </Button>
          <button
            type="button"
            disabled={sent}
            className="text-muted-foreground text-xs underline-offset-2 hover:underline"
            onClick={() => {
              say("/steps off");
              go(skip.length ? { skip } : "go");
            }}
          >
            Don't show steps in this conversation
          </button>
        </div>
      </CardContent>
    </Card>
  );
}

export function AskCard({ a }: { a: ChatAsk }) {
  return a.kind === "fill" ? <FillCard key={a.id} a={a} /> : <StepsCard key={a.id} a={a} />;
}

// ---- failed tries, folded

export interface Fold {
  start: number;
  end: number;
  name: string;
  tries: number;
  denied: number;
  error: string;
}

function errorOf(result: string | undefined): string | null {
  if (result === undefined) return null;
  try {
    const v = JSON.parse(result);
    return v && typeof v === "object" && "error" in v ? String(v.error) : null;
  } catch {
    return null;
  }
}

/** Runs of the same tool failing (and being denied) again and again, by where they start. */
export function foldFailures(messages: ChatMessage[], results: Map<string, string>): Map<number, Fold> {
  const out = new Map<number, Fold>();
  // An assistant round that's only failed calls of one tool: that tool's name.
  const failedRound = (m: ChatMessage) => {
    if (m.role !== "assistant" || m.content.trim() || !m.calls?.length) return null;
    const name = m.calls[0].name;
    return m.calls.every((c) => c.name === name && errorOf(results.get(c.id)) !== null) ? name : null;
  };
  let i = 0;
  while (i < messages.length) {
    const name = failedRound(messages[i]);
    if (!name) {
      i++;
      continue;
    }
    let j = i;
    let tries = 0;
    let denied = 0;
    let error = "";
    let last = i;
    while (j < messages.length) {
      const m = messages[j];
      if (failedRound(m) === name) {
        tries += m.calls.length;
        error = errorOf(results.get(m.calls[m.calls.length - 1].id)) ?? error;
        last = j;
      } else if (m.role === "approval") {
        if (m.content.includes("→ denied")) denied++;
      } else if (m.role !== "tool" && m.role !== "agent_tool") break;
      j++;
    }
    if (tries >= 2) out.set(i, { start: i, end: last, name, tries, denied, error });
    i = last + 1;
  }
  return out;
}

/** "mail_send · tried 3 times: no draft", opening to the cards. */
export function FoldedTries({ f, children }: { f: Fold; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const short = f.error.split(/[:(]/)[0].trim();
  return (
    <div className="rounded-md border border-red-900/50 bg-red-950/10 text-xs">
      <button type="button" onClick={() => setOpen(!open)} className="flex w-full items-center gap-2 px-3 py-1.5 text-left">
        {open ? <ChevronDown className="size-3.5 shrink-0" /> : <ChevronRight className="size-3.5 shrink-0" />}
        <span className="font-mono text-red-300">{f.name}</span>
        <span className="min-w-0 flex-1 truncate text-muted-foreground">
          tried {f.tries} times{f.denied ? `, ${f.denied} denied` : ""}
          {short ? `: ${short}` : ""}
        </span>
      </button>
      {open && <div className="space-y-3 border-red-900/40 border-t p-3">{children}</div>}
    </div>
  );
}

// ---- the chat's controls

/** The composer's switch: no tools in this conversation. */
export function ChatOnlyButton() {
  const { status, say, connected } = useLyra();
  const on = !!status.chat_only;
  return (
    <PromptInputButton
      aria-label="Chat only"
      aria-pressed={on}
      title={on ? "Chat only: lyra answers in words, with no tools. Tap to use tools again." : "Chat only: just talk, no tools in this conversation"}
      disabled={!connected || !!status.waiting}
      onClick={() => say(`/chat-only ${on ? "off" : "on"}`)}
      className={cn(on && "bg-sky-900/50 text-sky-200 hover:bg-sky-900/70")}
    >
      <MessagesSquare className="size-4" />
      {on && <span className="text-xs">Chat only</span>}
    </PromptInputButton>
  );
}

/** Your last message, changed and sent again: lyra's answer to it is replaced. */
export function EditLast({ text, onDone }: { text: string; onDone: () => void }) {
  const { say } = useLyra();
  const [value, setValue] = useState(text);
  const send = () => {
    if (value.trim() && value.trim() !== text.trim() && say(`/edit ${value.trim()}`)) onDone();
  };
  return (
    <div className="w-full max-w-[80%] space-y-2 self-end">
      <Textarea
        autoFocus
        value={value}
        onChange={(e) => setValue(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            send();
          } else if (e.key === "Escape") onDone();
        }}
        className="min-h-20"
        aria-label="Your message"
      />
      <div className="flex justify-end gap-2">
        <Button size="sm" variant="ghost" onClick={onDone}>
          Cancel
        </Button>
        <Button size="sm" onClick={send} disabled={!value.trim() || value.trim() === text.trim()}>
          Send instead
        </Button>
      </div>
    </div>
  );
}

/** Under your last message: change it. */
export function EditButton({ onEdit }: { onEdit: () => void }) {
  return (
    <button type="button" onClick={onEdit} className="inline-flex items-center gap-1 self-end text-muted-foreground text-xs hover:text-foreground" aria-label="Edit your message">
      <Pencil className="size-3" /> Edit
    </button>
  );
}

/** Under the last reply: ask again, or ask the other model. */
export function RetryButtons() {
  const { status, say } = useLyra();
  if (status.waiting) return null;
  return (
    <>
      <button type="button" onClick={() => say("/retry")} className="inline-flex items-center gap-1 hover:text-foreground" title="Ask again: this reply is replaced">
        <RefreshCw className="size-3" /> Retry
      </button>
      {status.other_model && (
        <button type="button" onClick={() => say("/retry other")} className="inline-flex items-center gap-1 hover:text-foreground" title={`Ask ${status.other_model} instead: this reply is replaced`}>
          <RotateCcw className="size-3" /> Ask {status.other_model.replace(/\.gguf$/i, "")}
        </button>
      )}
    </>
  );
}
