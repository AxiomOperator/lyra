// The conversation: messages (AI Elements), approvals, pairing requests and
// the composer with its `/` (commands) and `@` (machines) palettes.

import { Conversation, ConversationContent, ConversationEmptyState, ConversationScrollButton } from "@/components/ai-elements/conversation";
import { Message, MessageContent, MessageResponse } from "@/components/ai-elements/message";
import { PromptInput, PromptInputBody, PromptInputFooter, PromptInputSubmit, PromptInputTextarea, PromptInputTools } from "@/components/ai-elements/prompt-input";
import { Reasoning, ReasoningContent, ReasoningTrigger } from "@/components/ai-elements/reasoning";
import { Shimmer } from "@/components/ai-elements/shimmer";
import { Tool, ToolContent, ToolHeader, ToolInput, ToolOutput } from "@/components/ai-elements/tool";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { AtSign, Bot, Link2, ShieldAlert, ShieldCheck, ShieldX, Slash, TriangleAlert } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useLyra } from "./store";
import type { Approval, ChatMessage, PairRequest } from "./types";

// ---- messages

function parse(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

/** One tool call, with its result when it has come back. */
function ToolCallView({ name, args, result }: { name: string; args: string; result?: string }) {
  const input = parse(args) as Record<string, unknown>;
  const output = result === undefined ? undefined : parse(result);
  const obj = output && typeof output === "object" ? (output as Record<string, unknown>) : null;
  const error = obj && "error" in obj ? String(obj.error) : undefined;
  const state = result === undefined ? "input-available" : error ? "output-error" : "output-available";
  // What it's about, at a glance: the command, path or URL.
  const subject = [input?.command, input?.path, input?.url, input?.query].find((v) => typeof v === "string") as string | undefined;
  const where = typeof input?.machine === "string" && input.machine !== "server" ? ` @${input.machine}` : "";
  const title = `${name}${where}${subject ? ` · ${subject.length > 70 ? subject.slice(0, 69) + "…" : subject}` : ""}`;
  const text = (v: unknown) => (typeof v === "string" ? v : "");
  return (
    <Tool className="mb-0 bg-background/40">
      <ToolHeader type="dynamic-tool" toolName={name} title={title} state={state} className="text-left [&_span]:[overflow-wrap:anywhere]" />
      <ToolContent>
        <ToolInput input={input} />
        {error && <div className="rounded-md bg-red-950/40 p-3 font-mono text-red-300 text-xs whitespace-pre-wrap break-words">{error}</div>}
        {obj && !error && ("stdout" in obj || "stderr" in obj) && (
          <div className="space-y-2">
            <h4 className="font-medium text-muted-foreground text-xs uppercase tracking-wide">
              Output · exit {String(obj.exit_code ?? "?")}
              {obj.timed_out ? " · timed out" : ""}
              {typeof obj.seconds === "number" ? ` · ${obj.seconds}s` : ""}
            </h4>
            {text(obj.stdout) && <pre className="max-h-80 overflow-auto rounded-md bg-black/40 p-3 font-mono text-xs whitespace-pre-wrap break-words">{text(obj.stdout)}</pre>}
            {text(obj.stderr) && <pre className="max-h-48 overflow-auto rounded-md bg-red-950/30 p-3 font-mono text-red-300 text-xs whitespace-pre-wrap break-words">{text(obj.stderr)}</pre>}
          </div>
        )}
        {obj && !error && !("stdout" in obj || "stderr" in obj) && typeof obj.content === "string" && (
          <pre className="max-h-80 overflow-auto rounded-md bg-black/40 p-3 font-mono text-xs whitespace-pre-wrap break-words">{obj.content}</pre>
        )}
        {output !== undefined && !error && !(obj && ("stdout" in obj || "stderr" in obj || typeof obj.content === "string")) && <ToolOutput output={output as never} errorText={undefined} />}
      </ToolContent>
    </Tool>
  );
}

function Footer({ m }: { m: ChatMessage }) {
  if (!m.stats && !m.agents?.length && !m.skills?.length) return null;
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-muted-foreground text-xs">
      {m.stats && <span>{m.stats}</span>}
      {m.agents?.length > 0 && <span className="text-sky-400">handled with {m.agents.join(", ")}</span>}
      {m.skills?.length > 0 && <span className="text-fuchsia-400">skills: {m.skills.join(", ")}</span>}
    </div>
  );
}

function MessageView({ m, results, streaming }: { m: ChatMessage; results: Map<string, string>; streaming: boolean }) {
  switch (m.role) {
    case "user":
      return (
        <Message from="user">
          <MessageContent className="whitespace-pre-wrap">{m.content}</MessageContent>
        </Message>
      );
    case "assistant":
      return (
        <Message from="assistant">
          <MessageContent className="w-full">
            {m.reasoning?.trim() && (
              <Reasoning isStreaming={streaming && !m.content} defaultOpen={false}>
                <ReasoningTrigger />
                <ReasoningContent>{m.reasoning.trim()}</ReasoningContent>
              </Reasoning>
            )}
            {m.content && <MessageResponse isAnimating={streaming}>{m.content}</MessageResponse>}
            {m.calls?.map((c) => <ToolCallView key={c.id} name={c.name} args={c.arguments} result={results.get(c.id)} />)}
            <Footer m={m} />
          </MessageContent>
        </Message>
      );
    case "agent": {
      // One card per delegation: "working on it", its tool calls as they
      // happen, then how it went.
      const working = m.content.includes("· working on it");
      const [head, ...rest] = m.content.split("\n");
      return (
        <div className="space-y-3 rounded-lg border border-sky-900/60 bg-sky-950/30 p-3 text-sm">
          <div className="flex items-start gap-3">
            <Bot className="mt-0.5 size-4 shrink-0 text-sky-400" />
            <div className="min-w-0 flex-1">
              {working ? <Shimmer className="text-sky-200">{head}</Shimmer> : <div className="font-medium text-sky-200">{head}</div>}
            </div>
          </div>
          {m.calls?.length > 0 && (
            <div className="space-y-2">
              {m.calls.map((c) => (
                <ToolCallView key={c.id} name={c.name} args={c.arguments} result={results.get(c.id)} />
              ))}
            </div>
          )}
          {rest.join("\n").trim() && (
            <div className="text-sky-100/90">
              <MessageResponse>{rest.join("\n")}</MessageResponse>
            </div>
          )}
        </div>
      );
    }
    case "approval": {
      const denied = m.content.includes("→ denied");
      const Icon = denied ? ShieldX : ShieldCheck;
      return (
        <div className={cn("flex gap-3 rounded-lg border p-3 text-sm", denied ? "border-red-900/60 bg-red-950/20" : "border-emerald-900/60 bg-emerald-950/20")}>
          <Icon className={cn("mt-0.5 size-4 shrink-0", denied ? "text-red-400" : "text-emerald-400")} />
          <div className="min-w-0 flex-1 whitespace-pre-wrap break-words text-muted-foreground">{m.content}</div>
        </div>
      );
    }
    case "error":
      return (
        <Alert variant="destructive">
          <TriangleAlert />
          <AlertDescription className="whitespace-pre-wrap break-words">{m.content}</AlertDescription>
        </Alert>
      );
    case "tool":
    case "agent_tool":
      // Shown with its call; a stray one (no call to attach to) stays small.
      return <div className="truncate font-mono text-muted-foreground text-xs">↳ {m.content}</div>;
    default:
      return <div className="whitespace-pre-wrap break-words rounded-lg bg-muted/40 px-3 py-2 font-mono text-muted-foreground text-xs leading-relaxed">{m.content}</div>;
  }
}

// ---- cards

export function ApprovalCard({ a, more }: { a: Approval; more: number }) {
  const { send } = useLyra();
  const [sent, setSent] = useState(false);
  useEffect(() => setSent(false), [a.id]);
  const answer = (answer: string) => {
    setSent(true);
    send({ type: "approve", id: a.id, answer });
  };
  return (
    <Card className={cn("mx-3 mb-2 gap-3 border-2 py-4", a.dangerous ? "border-red-500/70 bg-red-950/30" : "border-amber-500/70 bg-amber-950/20")}>
      <CardContent className="space-y-3 px-4">
        <div className="flex items-start gap-2">
          <ShieldAlert className={cn("mt-0.5 size-5 shrink-0", a.dangerous ? "text-red-400" : "text-amber-400")} />
          <div className="font-medium">
            <span className="text-sky-400">{a.agent}</span> wants to {a.what}
          </div>
        </div>
        <pre className="max-h-[30vh] overflow-auto whitespace-pre-wrap break-words rounded-md bg-black/40 p-3 font-mono text-cyan-300 text-sm">{a.detail}</pre>
        <div className={cn("text-sm", a.dangerous ? "text-red-300" : "text-amber-300")}>
          <span className="font-semibold">{a.dangerous ? "Risk: " : "Why it asks: "}</span>
          {a.why}
        </div>
        <div className="grid grid-cols-3 gap-2">
          <Button disabled={sent} onClick={() => answer("y")} className="bg-emerald-600 text-white hover:bg-emerald-500">Allow once</Button>
          <Button disabled={sent} onClick={() => answer("n")} variant="destructive">Deny</Button>
          <Button disabled={sent} onClick={() => answer("a")} variant="outline">For session</Button>
        </div>
        {more > 0 && <div className="text-muted-foreground text-xs">{more} more waiting</div>}
      </CardContent>
    </Card>
  );
}

export function PairCard({ p, className }: { p: PairRequest; className?: string }) {
  const { send } = useLyra();
  const [sent, setSent] = useState(false);
  const answer = (approve: boolean) => {
    setSent(true);
    send({ type: "pair_answer", id: p.id, approve });
  };
  return (
    <Card className={cn("gap-3 border-2 border-teal-500/60 bg-teal-950/20 py-4", className)}>
      <CardContent className="space-y-3 px-4">
        <div className="flex items-start gap-2">
          <Link2 className="mt-0.5 size-5 shrink-0 text-teal-400" />
          <div>
            <span className="font-semibold">{p.name}</span>{" "}
            <span className="text-muted-foreground">
              ({p.hostname || "?"}
              {p.os ? `, ${p.os}` : ""})
            </span>{" "}
            asks to pair as a {p.kind === "node" ? "machine" : "device"}.
          </div>
        </div>
        <div className="text-muted-foreground text-sm">
          Check it shows this code: <span className="font-mono text-lg text-teal-300 tracking-[0.2em]">{p.code}</span>
        </div>
        <div className="grid grid-cols-2 gap-2">
          <Button disabled={sent} onClick={() => answer(true)} className="bg-emerald-600 text-white hover:bg-emerald-500">Approve</Button>
          <Button disabled={sent} onClick={() => answer(false)} variant="destructive">Deny</Button>
        </div>
      </CardContent>
    </Card>
  );
}

// ---- composer palettes

interface Entry {
  label: string;
  description: string;
  completion: string;
  mention: boolean;
}

function usePalette(text: string): Entry[] {
  const { commands, status } = useLyra();
  return useMemo(() => {
    const word = text.split(/\s/).pop() ?? "";
    if (word.startsWith("@")) {
      const typed = word.slice(1).toLowerCase();
      const before = text.slice(0, text.length - word.length);
      const list: Entry[] = [{ label: "@server", description: "where lyra runs", completion: `${before}@server `, mention: true }];
      for (const m of status.machines_detail ?? []) {
        if (!m.online) continue;
        list.push({ label: `@${m.name}`, description: ["online", m.hostname, m.os].filter(Boolean).join(" · "), completion: `${before}@${m.name} `, mention: true });
      }
      return list.filter((e) => e.label.slice(1).toLowerCase().startsWith(typed));
    }
    if (!text.startsWith("/")) return [];
    const q = text.toLowerCase();
    let found = commands.filter((c) => c.usage.toLowerCase().startsWith(q));
    if (!found.length && q.includes(" ")) {
      const first = q.split(/\s+/)[0];
      found = commands.filter((c) => c.usage.split(/\s+/)[0] === first);
    }
    return found.slice(0, 40).map((c) => ({ label: c.usage, description: c.description, completion: c.completion, mention: false }));
  }, [text, commands, status.machines_detail]);
}

function Composer() {
  const { say, send, status } = useLyra();
  const [text, setText] = useState("");
  const [selected, setSelected] = useState(0);
  const [hidden, setHidden] = useState(false);
  const entries = usePalette(hidden ? "" : text);
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => setSelected(0), [text]);
  // "Ask on @desktop" from the Machines page.
  useEffect(() => {
    const fill = (e: Event) => setText(`@${(e as CustomEvent<string>).detail} `);
    window.addEventListener("lyra-mention", fill);
    return () => window.removeEventListener("lyra-mention", fill);
  }, []);
  useEffect(() => {
    listRef.current?.querySelector(`[data-i="${selected}"]`)?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  const complete = (e: Entry) => {
    setText(e.completion);
    setHidden(false);
    if (!e.mention && !e.completion.endsWith(" ")) {
      if (say(e.completion)) setText("");
    }
  };
  const submit = () => {
    const t = text.trim();
    if (!t) return;
    if (say(t)) setText("");
  };
  const onKeyDown = (e: KeyboardEvent<HTMLTextAreaElement>) => {
    if (!entries.length) return;
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      setSelected((s) => (s + (e.key === "ArrowDown" ? 1 : entries.length - 1)) % entries.length);
    } else if (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey && (entries[0].mention || (!text.includes(" ") && !entries.some((x) => x.label.split(" ")[0] === text.trim()))))) {
      e.preventDefault();
      complete(entries[Math.min(selected, entries.length - 1)]);
    } else if (e.key === "Escape") {
      e.preventDefault();
      setHidden(true);
    }
  };

  return (
    <div className="relative px-3 pb-3">
      {entries.length > 0 && (
        <div ref={listRef} className="absolute right-3 bottom-full left-3 mb-2 max-h-72 overflow-y-auto rounded-xl border bg-popover p-1 shadow-xl">
          <div className="flex items-center gap-1.5 px-2 py-1 text-muted-foreground text-xs">
            {entries[0].mention ? <AtSign className="size-3" /> : <Slash className="size-3" />}
            {entries[0].mention ? "Machines" : "Commands"} · ↑↓ Tab Esc
          </div>
          {entries.map((e, i) => (
            <button
              key={e.label}
              data-i={i}
              type="button"
              onMouseDown={(ev) => ev.preventDefault()}
              onClick={() => complete(e)}
              className={cn("flex w-full flex-col items-start rounded-lg px-2 py-1.5 text-left", i === selected ? "bg-accent" : "hover:bg-accent/60")}
            >
              <span className="font-mono text-sm text-teal-300">{e.label}</span>
              {e.description && <span className="text-muted-foreground text-xs">{e.description}</span>}
            </button>
          ))}
        </div>
      )}
      <PromptInput onSubmit={submit}>
        <PromptInputBody>
          <PromptInputTextarea
            value={text}
            onChange={(e) => {
              setText(e.currentTarget.value);
              setHidden(false);
            }}
            onKeyDown={onKeyDown}
            placeholder="Message lyra…  ( / commands · @ machines )"
          />
        </PromptInputBody>
        <PromptInputFooter>
          <PromptInputTools>
            <span className="px-2 text-muted-foreground text-xs">{status.model}</span>
          </PromptInputTools>
          {/* While a reply is being written the button stops it. */}
          <PromptInputSubmit disabled={!status.waiting && !text.trim()} status={status.waiting ? "streaming" : undefined} onStop={() => send({ type: "stop" })} />
        </PromptInputFooter>
      </PromptInput>
    </div>
  );
}

// ---- the page

export function ChatPage() {
  const { messages, status, ready } = useLyra();
  // Tool results, by the call they answer.
  const results = useMemo(() => {
    const map = new Map<string, string>();
    for (const m of messages) if ((m.role === "tool" || m.role === "agent_tool") && m.tool_call_id) map.set(m.tool_call_id, m.content);
    return map;
  }, [messages]);
  const attached = useMemo(() => new Set(messages.flatMap((m) => (m.calls ?? []).map((c) => c.id))), [messages]);
  const approvals = status.approvals ?? [];
  const pairing = status.pairing ?? [];
  const last = messages[messages.length - 1];
  // An agent's card shows its own progress; this is for lyra itself.
  const thinking = status.waiting && (!last || ["user", "tool", "approval"].includes(last.role)) && !status.phase?.startsWith("↪");

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <Conversation className="min-h-0 flex-1">
        <ConversationContent className="gap-5 px-3 py-4">
          {ready && messages.length === 0 && <ConversationEmptyState title="Ask lyra anything" description="Type / for commands, @ to pick a machine." />}
          {messages.map((m, i) =>
            (m.role === "tool" || m.role === "agent_tool") && m.tool_call_id && attached.has(m.tool_call_id) ? null : (
              <MessageView key={i} m={m} results={results} streaming={!!status.waiting && i === messages.length - 1} />
            ),
          )}
          {thinking && <Shimmer className="text-sm">Thinking…</Shimmer>}
        </ConversationContent>
        <ConversationScrollButton />
      </Conversation>
      {pairing.map((p) => (
        <PairCard key={p.id} p={p} className="mx-3 mb-2" />
      ))}
      {approvals[0] && <ApprovalCard a={approvals[0]} more={approvals.length - 1} />}
      <Composer />
    </div>
  );
}
