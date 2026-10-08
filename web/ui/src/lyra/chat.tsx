// The conversation: messages (AI Elements), approvals, pairing requests and
// the composer with its `/` (commands) and `@` (machines) palettes.

import { Conversation, ConversationContent, ConversationEmptyState, ConversationScrollButton } from "@/components/ai-elements/conversation";
import { Message, MessageContent, MessageResponse } from "@/components/ai-elements/message";
import { PromptInput, PromptInputBody, PromptInputButton, PromptInputFooter, PromptInputHeader, PromptInputSubmit, PromptInputTextarea, PromptInputTools } from "@/components/ai-elements/prompt-input";
import { SpeechInput } from "@/components/ai-elements/speech-input";
import { CodingCard } from "./coding";
import { withDictation } from "./dictation";
import { Reasoning, ReasoningContent, ReasoningTrigger } from "@/components/ai-elements/reasoning";
import { Shimmer } from "@/components/ai-elements/shimmer";
import { Tool, ToolContent, ToolHeader, ToolInput, ToolOutput } from "@/components/ai-elements/tool";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { AtSign, Link2, Paperclip, ShieldAlert, ShieldCheck, ShieldX, Slash, TriangleAlert } from "lucide-react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import { useLyra } from "./store";
import type { Approval, ChatMessage, PairRequest } from "./types";
import { ComposerModel, PlanCard, ToolFileContent, ToolFileTree, ToolTerminal, type PlanView } from "./chat-parts2";
import { AgentTask, ApprovalAt, ComposerAttachments, ReplyContext, ReplySources, SentAttachments, StarterSuggestions, approvalCall, splitAttached, webSources } from "./chat-parts";

/** The approval waiting, and the call it's shown at. */
type Asking = { callId: string; a: Approval; more: number } | null;

// ---- messages

function parse(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch {
    return text;
  }
}

/** One tool call, with its result when it has come back. */
interface FleetResult {
  machine: string;
  ok: boolean;
  exit_code?: number | null;
  stdout?: string;
  stderr?: string;
  error?: string;
  seconds?: number;
}

/** One machine's part of a run on several. */
function MachineResult({ r }: { r: FleetResult }) {
  const [open, setOpen] = useState(!r.ok);
  return (
    <div className={cn("rounded-md border", r.ok ? "border-border" : "border-red-800/60")}>
      <button type="button" onClick={() => setOpen(!open)} className="flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left text-xs">
        <span className={cn("font-medium", r.ok ? "text-emerald-300" : "text-red-300")}>
          {r.ok ? "✓" : "✗"} {r.machine}
        </span>
        <span className="text-muted-foreground">
          {r.error ? "not run" : `exit ${r.exit_code ?? "?"}`}
          {typeof r.seconds === "number" ? ` · ${r.seconds}s` : ""}
        </span>
      </button>
      {open && (
        <div className="space-y-1.5 px-3 pb-2">
          {r.error && <pre className="whitespace-pre-wrap break-words font-mono text-red-300 text-xs">{r.error}</pre>}
          {r.stdout && <pre className="max-h-60 overflow-auto whitespace-pre-wrap break-words rounded bg-black/40 p-2 font-mono text-xs">{r.stdout}</pre>}
          {r.stderr && <pre className="max-h-40 overflow-auto whitespace-pre-wrap break-words rounded bg-red-950/30 p-2 font-mono text-red-300 text-xs">{r.stderr}</pre>}
        </div>
      )}
    </div>
  );
}

function ToolCallView({ name, args, result }: { name: string; args: string; result?: string }) {
  // Coding work has its own card: live steps, then what changed.
  if (name === "code_task") return <CodingCard args={parse(args) as Record<string, unknown>} result={result} />;
  const input = parse(args) as Record<string, unknown>;
  const output = result === undefined ? undefined : parse(result);
  const obj = output && typeof output === "object" ? (output as Record<string, unknown>) : null;
  const error = obj && "error" in obj ? String(obj.error) : undefined;
  const state = result === undefined ? "input-available" : error ? "output-error" : "output-available";
  // What it's about, at a glance: the command, path or URL.
  const subject = [input?.command, input?.path, input?.url, input?.query].find((v) => typeof v === "string") as string | undefined;
  const where = typeof input?.machine === "string" && input.machine !== "server" ? ` @${input.machine}` : typeof input?.machines === "string" ? ` @${input.machines}` : "";
  // One run on several machines: a card per machine.
  const fleet = obj && Array.isArray(obj.results) ? (obj.results as FleetResult[]) : null;
  const title = `${name}${where}${subject ? ` · ${subject.length > 70 ? subject.slice(0, 69) + "…" : subject}` : ""}`;
  const text = (v: unknown) => (typeof v === "string" ? v : "");
  const listing = !!obj && Array.isArray(obj.entries) && (name === "file_list" || name === "project_list");
  const fileText = obj && (name === "file_read" || name === "project_read") ? (typeof obj.content === "string" ? obj.content : typeof obj.text === "string" ? obj.text : null) : null;
  const shown = !!obj && ("stdout" in obj || "stderr" in obj || listing || fileText !== null || typeof obj.content === "string");
  return (
    <Tool className="mb-0 bg-background/40">
      <ToolHeader type="dynamic-tool" toolName={name} title={title} state={state} className="text-left [&_span]:[overflow-wrap:anywhere]" />
      <ToolContent>
        <ToolInput input={input} />
        {error && <div className="rounded-md bg-red-950/40 p-3 font-mono text-red-300 text-xs whitespace-pre-wrap break-words">{error}</div>}
        {fleet && (
          <div className="space-y-2">
            <h4 className="font-medium text-muted-foreground text-xs uppercase tracking-wide">
              {String(obj?.machines)} machines · {String(obj?.ok)} ok · {String(obj?.failed)} failed
            </h4>
            {fleet.map((r) => (
              <MachineResult key={r.machine} r={r} />
            ))}
          </div>
        )}
        {/* A command's output: a terminal. */}
        {obj && !error && !fleet && ("stdout" in obj || "stderr" in obj) && (
          <ToolTerminal title="Output" stdout={text(obj.stdout)} stderr={text(obj.stderr)} exit={obj.exit_code} seconds={typeof obj.seconds === "number" ? obj.seconds : undefined} timedOut={!!obj.timed_out} />
        )}
        {/* A folder listing: a tree. */}
        {obj && !error && listing && <ToolFileTree entries={obj.entries as never} cut={!!(obj.truncated || obj.cut)} />}
        {/* A file's words: highlighted when it's code. */}
        {obj && !error && fileText !== null && <ToolFileContent path={String(input?.path ?? obj.path ?? "")} text={fileText} cut={!!(obj.truncated || obj.cut)} />}
        {obj && !error && !fleet && fileText === null && !("stdout" in obj || "stderr" in obj) && typeof obj.content === "string" && (
          <pre className="max-h-80 overflow-auto rounded-md bg-black/40 p-3 font-mono text-xs whitespace-pre-wrap break-words">{obj.content}</pre>
        )}
        {output !== undefined && !error && !fleet && !shown && <ToolOutput output={output as never} errorText={undefined} />}
      </ToolContent>
    </Tool>
  );
}

function Footer({ m, max, model }: { m: ChatMessage; max: number; model?: string }) {
  if (!m.stats && !m.agents?.length && !m.skills?.length) return null;
  return (
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-muted-foreground text-xs">
      {m.usage && <ReplyContext usage={m.usage} max={max} model={model} />}
      {m.stats && <span>{m.stats}</span>}
      {m.agents?.length > 0 && <span className="text-sky-400">handled with {m.agents.join(", ")}</span>}
      {m.skills?.length > 0 && <span className="text-fuchsia-400">skills: {m.skills.join(", ")}</span>}
    </div>
  );
}

function MessageView({ m, results, streaming, asking, max, model, turn }: { m: ChatMessage; results: Map<string, string>; streaming: boolean; asking: Asking; max: number; model?: string; turn?: ChatMessage[] }) {
  // A call, and the approval it waits for when it's this one.
  const call = (c: ChatMessage["calls"][number]) => (
    <div key={c.id} className="space-y-2">
      <ToolCallView name={c.name} args={c.arguments} result={results.get(c.id)} />
      {asking?.callId === c.id && <ApprovalAt a={asking.a} more={asking.more} />}
    </div>
  );
  switch (m.role) {
    case "user": {
      const { text, files } = splitAttached(m.content);
      return (
        <Message from="user">
          <SentAttachments files={files} />
          {text.trim() && <MessageContent className="whitespace-pre-wrap">{text}</MessageContent>}
        </Message>
      );
    }
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
            {m.calls?.map(call)}
            {/* The turn's sources, under its answer. */}
            {!streaming && turn && <ReplySources web={webSources(turn, results)} memories={m.memory_notes ?? []} />}
            <Footer m={m} max={max} model={model} />
          </MessageContent>
        </Message>
      );
    case "agent": {
      // One card per delegation: "working on it", its tool calls as they
      // happen, then how it went.
      const working = m.content.includes("· working on it");
      const [head, ...rest] = m.content.split("\n");
      return (
        <AgentTask head={head} working={working} steps={m.calls?.length ? m.calls.map(call) : <div className="text-muted-foreground text-xs">{working ? "starting…" : "no steps"}</div>}>
          {rest.join("\n").trim() && (
            <div className="mt-3 text-sky-100/90">
              <MessageResponse>{rest.join("\n")}</MessageResponse>
            </div>
          )}
        </AgentTask>
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
  const { send, user } = useLyra();
  const [sent, setSent] = useState(false);
  // A terminal acts as someone: approved here, it's yours.
  const terminal = p.kind !== "node";
  const answer = (approve: boolean) => {
    setSent(true);
    send({ type: "pair_answer", id: p.id, approve, ...(terminal && approve && user ? { user: user.user } : {}) });
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
        {terminal && (
          <div className="text-amber-300 text-sm">
            Approved here, this terminal signs in as you ({user?.name ?? "your account"}). For a coworker's terminal, use /devices approve {p.code} for &lt;their email&gt;.
          </div>
        )}
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
      // Several at once: every machine, or a group (one approval, a result each).
      const online = (status.machines_detail ?? []).filter((m) => m.online).length;
      if (online > 0) list.push({ label: "@all", description: `the server and ${online} machine${online === 1 ? "" : "s"} at once`, completion: `${before}@all `, mention: true });
      for (const g of status.groups ?? []) {
        list.push({ label: `@${g.name}`, description: `group: ${g.machines.join(", ")}`, completion: `${before}@${g.name} `, mention: true });
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
  }, [text, commands, status.machines_detail, status.groups]);
}

/** Can this browser turn speech into text itself? (no server transcription) */
const canDictate = typeof window !== "undefined" && ("SpeechRecognition" in window || "webkitSpeechRecognition" in window);

/** Send a file to lyra; returns its upload id. */
async function upload(token: string, file: File): Promise<string> {
  const r = await fetch("/api/files", {
    method: "POST",
    headers: { Authorization: `Bearer ${token}`, "Content-Type": file.type || "application/octet-stream", "X-Filename": encodeURIComponent(file.name) },
    body: file,
  });
  const body = await r.json().catch(() => ({}));
  if (!r.ok) throw new Error(body.error || r.statusText);
  return body.id as string;
}

function Composer() {
  const { say, send, status, token, user } = useLyra();
  const [text, setText] = useState("");
  // The message as typed when dictation started; what's said goes after it.
  const typedBefore = useRef("");
  const [files, setFiles] = useState<File[]>([]);
  const [busy, setBusy] = useState("");
  const fileInput = useRef<HTMLInputElement>(null);
  // Shared from another app (Android's share sheet): text and files.
  useEffect(() => {
    const take = (e: Event) => {
      const d = (e as CustomEvent<{ text: string; files: File[] }>).detail;
      if (d.text) setText((t) => (t ? `${t}\n${d.text}` : d.text));
      if (d.files?.length) setFiles((f) => [...f, ...d.files]);
    };
    window.addEventListener("lyra-share", take);
    return () => window.removeEventListener("lyra-share", take);
  }, []);
  const [selected, setSelected] = useState(0);
  const [hidden, setHidden] = useState(false);
  const entries = usePalette(hidden ? "" : text);
  const listRef = useRef<HTMLDivElement>(null);
  useEffect(() => setSelected(0), [text]);
  // "Ask on @desktop" from the Machines page.
  useEffect(() => {
    const fill = (e: Event) => setText(`@${(e as CustomEvent<string>).detail} `);
    window.addEventListener("lyra-mention", fill);
    // A card's "Continue": text to finish typing.
    const prefill = (e: Event) => setText((e as CustomEvent<string>).detail);
    window.addEventListener("lyra-prefill", prefill);
    return () => {
      window.removeEventListener("lyra-mention", fill);
      window.removeEventListener("lyra-prefill", prefill);
    };
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
  const submit = async () => {
    const t = text.trim();
    if (!t && !files.length) return;
    let ids: string[] = [];
    if (files.length) {
      try {
        for (const [i, f] of files.entries()) {
          setBusy(`Sending ${f.name} (${i + 1}/${files.length})…`);
          ids.push(await upload(token, f));
        }
      } catch (e) {
        setBusy(`Couldn't send the file: ${(e as Error).message}`);
        ids = [];
        return;
      }
      setBusy("");
    }
    if (send({ type: "send", text: t, files: ids })) {
      setText("");
      setFiles([]);
    }
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
      <input
        ref={fileInput}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          const picked = Array.from(e.currentTarget.files ?? []);
          setFiles((f) => [...f, ...picked]);
          e.currentTarget.value = "";
        }}
      />
      <PromptInput onSubmit={() => void submit()}>
        {(files.length > 0 || busy) && (
          <PromptInputHeader className="flex flex-wrap gap-1.5 px-3 pt-2">
            <ComposerAttachments files={files} remove={(i) => setFiles((all) => all.filter((_, j) => j !== i))} />
            {busy && <span className="w-full text-muted-foreground text-xs">{busy}</span>}
          </PromptInputHeader>
        )}
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
            <PromptInputButton aria-label="Attach files" onClick={() => fileInput.current?.click()}>
              <Paperclip className="size-4" />
            </PromptInputButton>
            {canDictate && (
              <SpeechInput
                size="icon-sm"
                variant="ghost"
                onListenStart={() => (typedBefore.current = text)}
                onTranscriptionChange={(said) => setText(withDictation(typedBefore.current, said))}
              />
            )}
            <ComposerModel admin={user?.admin ?? true} />
          </PromptInputTools>
          {/* While a reply is being written the button stops it. */}
          <PromptInputSubmit disabled={!status.waiting && !text.trim() && !files.length} status={status.waiting ? "streaming" : undefined} onStop={() => send({ type: "stop" })} />
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
  const { user } = useLyra();
  // The first approval shows at its tool call; one with no call here (another
  // machine's, a plan's) waits above the composer as before.
  const callId = approvalCall(messages, results, approvals[0]);
  const asking: Asking = callId && approvals[0] ? { callId, a: approvals[0], more: approvals.length - 1 } : null;
  const max = status.context_window ?? 0;
  // Each turn's answer (its last reply with words) gets the turn's messages, for its sources.
  const turns = useMemo(() => {
    const out = new Map<number, ChatMessage[]>();
    let start = 0;
    const close = (end: number) => {
      const turn = messages.slice(start, end);
      for (let j = end - 1; j >= start; j--)
        if (messages[j].role === "assistant" && messages[j].content.trim()) {
          out.set(j, turn);
          break;
        }
    };
    messages.forEach((m, i) => {
      if (m.role === "user" && i > start) {
        close(i);
        start = i;
      }
    });
    close(messages.length);
    return out;
  }, [messages]);
  const pairing = status.pairing ?? [];
  const last = messages[messages.length - 1];
  // An agent's card shows its own progress; this is for lyra itself.
  const thinking = status.waiting && (!last || ["user", "tool", "approval"].includes(last.role)) && !status.phase?.startsWith("↪");

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <Conversation className="min-h-0 flex-1">
        <ConversationContent className="mx-auto w-full max-w-3xl gap-5 px-3 py-4 md:px-6 md:py-6">
          {ready && messages.length === 0 && (
            <div className="flex flex-1 flex-col items-center justify-center gap-6">
              <ConversationEmptyState className="flex-none" title="Ask lyra anything" description="Type / for commands, @ to pick a machine." />
              <StarterSuggestions admin={user?.admin ?? true} />
            </div>
          )}
          {messages.map((m, i) =>
            (m.role === "tool" || m.role === "agent_tool") && m.tool_call_id && attached.has(m.tool_call_id) ? null : (
              <MessageView key={i} m={m} results={results} streaming={!!status.waiting && i === messages.length - 1} asking={asking} max={max} model={status.model} turn={turns.get(i)} />
            ),
          )}
          {thinking && <Shimmer className="text-sm">Thinking…</Shimmer>}
        </ConversationContent>
        <ConversationScrollButton />
      </Conversation>
      {/* Approvals, pairing and the composer line up with the messages. */}
      <div className="mx-auto w-full max-w-3xl md:px-3">
        {pairing.map((p) => (
          <PairCard key={p.id} p={p} className="mx-3 mb-2" />
        ))}
        {!!status.plan && (user?.admin ?? true) && <PlanCard plan={status.plan as PlanView} />}
        {approvals[0] && !asking && <ApprovalCard a={approvals[0]} more={approvals.length - 1} />}
        <Composer />
      </div>
    </div>
  );
}
