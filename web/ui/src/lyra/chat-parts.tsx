// The chat's AI Elements pieces: attachments (in the composer and on sent
// messages), approvals at the tool call that asks (Confirmation), an agent's
// work (Task), the sources and memories a reply used (Sources), how much of
// the model's context a reply took and what it cost (Context), and starter
// prompts on an empty chat (Suggestion).

import { Attachment, AttachmentInfo, AttachmentPreview, AttachmentRemove, Attachments, type AttachmentData } from "@/components/ai-elements/attachments";
import { Confirmation, ConfirmationAction, ConfirmationActions, ConfirmationRequest, ConfirmationTitle } from "@/components/ai-elements/confirmation";
import { Context, ContextContent, ContextContentBody, ContextContentFooter, ContextContentHeader, ContextTrigger } from "@/components/ai-elements/context";
import { Source, Sources, SourcesContent, SourcesTrigger } from "@/components/ai-elements/sources";
import { Suggestion } from "@/components/ai-elements/suggestion";
import { Task, TaskContent, TaskTrigger } from "@/components/ai-elements/task";
import { Shimmer } from "@/components/ai-elements/shimmer";
import { cn } from "@/lib/utils";
import { Brain, ChevronDown, Globe, ShieldAlert } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { agentLook } from "./agents";
import { useLyra } from "./store";
import type { Approval, ChatMessage } from "./types";

// ---- attachments

/** A file in the composer, before it's sent. */
export function ComposerAttachments({ files, remove }: { files: File[]; remove: (i: number) => void }) {
  const [urls, setUrls] = useState<string[]>([]);
  // Pictures preview from the file itself.
  useEffect(() => {
    const made = files.map((f) => (f.type.startsWith("image/") ? URL.createObjectURL(f) : ""));
    setUrls(made);
    return () => made.forEach((u) => u && URL.revokeObjectURL(u));
  }, [files]);
  return (
    <Attachments variant="inline">
      {files.map((f, i) => (
        <Attachment key={`${f.name}-${i}`} data={{ id: `${i}`, type: "file", filename: f.name, mediaType: f.type || "application/octet-stream", url: urls[i] ?? "" }} onRemove={() => remove(i)}>
          <AttachmentPreview />
          <AttachmentInfo />
          <AttachmentRemove label={`Remove ${f.name}`} />
        </Attachment>
      ))}
    </Attachments>
  );
}

/** What lyra writes into a sent message about each file: `**Attached: name** (mime, size, upload `id`)…`. */
export interface Sent {
  id: string;
  name: string;
  mime: string;
}

/** A sent message's own words, and the files it carried. */
export function splitAttached(content: string): { text: string; files: Sent[] } {
  const parts = content.split("\n\n**Attached: ");
  const files: Sent[] = [];
  for (const p of parts.slice(1)) {
    const m = p.match(/^(.*?)\*\* \(([^,]+), [^,]+, upload `([^`]+)`\)/);
    if (m) files.push({ name: m[1], mime: m[2].trim(), id: m[3] });
  }
  return { text: files.length ? parts[0] : content, files };
}

/** An upload, fetched with this device's token (the server checks it's theirs). */
function useUpload(id: string, wanted: boolean) {
  const { token } = useLyra();
  const [url, setUrl] = useState("");
  useEffect(() => {
    if (!wanted) return;
    let made = "";
    let gone = false;
    void fetch(`/api/files/${encodeURIComponent(id)}`, { headers: { Authorization: `Bearer ${token}` } })
      .then((r) => (r.ok ? r.blob() : null))
      .then((b) => {
        if (b && !gone) {
          made = URL.createObjectURL(b);
          setUrl(made);
        }
      })
      .catch(() => {});
    return () => {
      gone = true;
      if (made) URL.revokeObjectURL(made);
    };
  }, [id, token, wanted]);
  return url;
}

function SentFile({ f }: { f: Sent }) {
  const image = f.mime.startsWith("image/");
  const url = useUpload(f.id, image);
  const data: AttachmentData = { id: f.id, type: "file", filename: f.name, mediaType: f.mime, url };
  return (
    <Attachment data={data}>
      <AttachmentPreview />
      <AttachmentInfo />
    </Attachment>
  );
}

/** The files a sent message carried. */
export function SentAttachments({ files, className }: { files: Sent[]; className?: string }) {
  if (!files.length) return null;
  return (
    <Attachments variant="inline" className={className ?? "justify-end"}>
      {files.map((f) => (
        <SentFile key={f.id} f={f} />
      ))}
    </Attachments>
  );
}

// ---- approvals, at the call that asks

/** What an approval would do: a diff (`--- now` / `+++ after`) in colour, else as it is. */
export function ApprovalDetail({ detail }: { detail: string }) {
  const cls = "max-h-[40vh] overflow-auto whitespace-pre-wrap break-words rounded-md bg-black/40 p-3 font-mono text-sm";
  if (!detail.startsWith("--- now")) return <pre className={cn(cls, "text-cyan-300")}>{detail}</pre>;
  return (
    <pre className={cls}>
      {detail
        .split("\n")
        .filter((l) => !l.startsWith("--- ") && !l.startsWith("+++ "))
        .map((l, i) => (
          <div key={i} className={cn(l.startsWith("+") ? "bg-emerald-950/60 text-emerald-300" : l.startsWith("-") ? "bg-red-950/60 text-red-300" : l.startsWith("@@") ? "text-sky-400/80" : "text-muted-foreground")}>
            {l || " "}
          </div>
        ))}
    </pre>
  );
}

/** The question an approval asks, with Allow once / Deny / For session, shown at its tool call. */
export function ApprovalAt({ a, more }: { a: Approval; more: number }) {
  const { send } = useLyra();
  const [sent, setSent] = useState(false);
  useEffect(() => setSent(false), [a.id]);
  const answer = (answer: string) => {
    setSent(true);
    send({ type: "approve", id: a.id, answer });
  };
  return (
    <Confirmation approval={{ id: String(a.id) }} state="approval-requested" className={cn("border-2", a.dangerous ? "border-red-500/70 bg-red-950/30" : "border-amber-500/70 bg-amber-950/20")}>
      <ConfirmationTitle>
        <span className="flex items-start gap-2 font-medium text-foreground">
          <ShieldAlert className={cn("mt-0.5 size-4 shrink-0", a.dangerous ? "text-red-400" : "text-amber-400")} />
          <span>
            <span className="text-sky-400">{a.agent}</span> wants to {a.what}
          </span>
        </span>
      </ConfirmationTitle>
      <ConfirmationRequest>
        {a.detail && <ApprovalDetail detail={a.detail} />}
        <div className={cn("text-sm", a.dangerous ? "text-red-300" : "text-amber-300")}>
          <span className="font-semibold">{a.dangerous ? "Risk: " : "Why it asks: "}</span>
          {a.why}
        </div>
      </ConfirmationRequest>
      <ConfirmationActions className="grid grid-cols-3 gap-2">
        <ConfirmationAction disabled={sent} onClick={() => answer("y")} className="bg-emerald-600 text-white hover:bg-emerald-500">
          Allow once
        </ConfirmationAction>
        <ConfirmationAction disabled={sent} onClick={() => answer("n")} variant="destructive">
          Deny
        </ConfirmationAction>
        <ConfirmationAction disabled={sent} onClick={() => answer("a")} variant="outline">
          For session
        </ConfirmationAction>
      </ConfirmationActions>
      {more > 0 && <div className="text-muted-foreground text-xs">{more} more waiting</div>}
    </Confirmation>
  );
}

/** Which call an approval belongs to: the latest unanswered call to its tool. */
export function approvalCall(messages: ChatMessage[], results: Map<string, string>, a: Approval | undefined): string | null {
  if (!a?.tool) return null;
  for (let i = messages.length - 1; i >= 0; i--) {
    const c = [...(messages[i].calls ?? [])].reverse().find((c) => c.name === a.tool && !results.has(c.id));
    if (c) return c.id;
  }
  return null;
}

// ---- an agent's work

/** One delegation: what it's doing (live), its steps, how it went. */
export function AgentTask({ head, working, steps, children, look }: { head: string; working: boolean; steps: ReactNode; children?: ReactNode; look?: { color?: string; icon?: string } }) {
  // The agent's own colour and icon (a huddle: the group's bot).
  const { colors, Icon } = agentLook(look);
  return (
    <Task defaultOpen className={cn("rounded-lg border p-3 text-sm", colors.card)}>
      <TaskTrigger title={head}>
        <div className="group flex w-full cursor-pointer items-start gap-2 text-left">
          <Icon className={cn("mt-0.5 size-4 shrink-0", colors.icon)} />
          <div className="min-w-0 flex-1">{working ? <Shimmer className={colors.text}>{head}</Shimmer> : <span className={cn("font-medium", colors.text)}>{head}</span>}</div>
          <ChevronDown className={cn("mt-0.5 size-4 shrink-0 opacity-70 transition-transform group-data-[state=open]:rotate-180", colors.icon)} />
        </div>
      </TaskTrigger>
      <TaskContent>
        <div className="mt-3 space-y-2 border-sky-900/60 border-l pl-3">{steps}</div>
        {children}
      </TaskContent>
    </Task>
  );
}

// ---- sources

function parse(text: string | undefined): Record<string, unknown> | null {
  if (!text) return null;
  try {
    const v = JSON.parse(text);
    return v && typeof v === "object" ? (v as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

/** The pages a reply read (web_fetch), else what its searches found (web_search),
 *  across the turn's messages (the search is one step, the answer the next). */
export function webSources(turn: ChatMessage[], results: Map<string, string>): { url: string; title: string }[] {
  const read: { url: string; title: string }[] = [];
  const found: { url: string; title: string }[] = [];
  for (const c of turn.flatMap((m) => m.calls ?? [])) {
    const r = parse(results.get(c.id));
    if (!r) continue;
    if (c.name === "web_fetch" && typeof r.url === "string") read.push({ url: r.url, title: (r.title as string) || r.url });
    if (c.name === "web_search" && Array.isArray(r.results))
      for (const x of r.results as { url?: string; title?: string }[]) if (x.url) found.push({ url: x.url, title: x.title || x.url });
  }
  const list = read.length ? read : found.slice(0, 6);
  return list.filter((s, i) => list.findIndex((t) => t.url === s.url) === i);
}

function host(url: string) {
  try {
    return new URL(url).host.replace(/^www\./, "");
  } catch {
    return url;
  }
}

/** Under a reply: the web pages it used and the memories lyra recalled for it. */
export function ReplySources({ web, memories }: { web: { url: string; title: string }[]; memories: { id: string; text: string }[] }) {
  return (
    <div className="flex flex-wrap gap-x-5 gap-y-2 text-xs">
      {web.length > 0 && (
        <Sources className="mb-0 text-muted-foreground">
          <SourcesTrigger count={web.length}>
            <Globe className="size-3.5" />
            <span>
              {web.length} source{web.length === 1 ? "" : "s"}
            </span>
            <ChevronDown className="size-3.5" />
          </SourcesTrigger>
          <SourcesContent className="w-full">
            {web.map((s) => (
              <Source key={s.url} href={s.url} title={s.title}>
                <Globe className="size-3.5 shrink-0" />
                <span className="truncate text-foreground">{s.title}</span>
                <span className="shrink-0 text-muted-foreground">{host(s.url)}</span>
              </Source>
            ))}
          </SourcesContent>
        </Sources>
      )}
      {memories.length > 0 && (
        <Sources className="mb-0 text-muted-foreground">
          <SourcesTrigger count={memories.length}>
            <Brain className="size-3.5" />
            <span>
              {memories.length} memor{memories.length === 1 ? "y" : "ies"}
            </span>
            <ChevronDown className="size-3.5" />
          </SourcesTrigger>
          <SourcesContent className="w-full">
            {memories.map((m) => (
              <div key={m.id} className="flex items-start gap-2">
                <Brain className="mt-0.5 size-3.5 shrink-0 text-fuchsia-400" />
                <span className="text-foreground">{m.text}</span>
                <span className="shrink-0 font-mono text-muted-foreground">{m.id}</span>
              </div>
            ))}
          </SourcesContent>
        </Sources>
      )}
    </div>
  );
}

// ---- context

export interface ReplyUsage {
  input: number;
  cached: number;
  output: number;
  ms: number;
  estimated: boolean;
  cost: number;
  currency: string;
}

const compact = (n: number) => new Intl.NumberFormat("en-US", { notation: "compact" }).format(n);

/** How much of the model's context a reply took, its tokens and its cost. */
export function ReplyContext({ usage, max, model }: { usage: ReplyUsage; max: number; model?: string }) {
  if (!max || usage.estimated) return null;
  const used = Math.min(usage.input + usage.output, max);
  const row = (label: string, value: string) => (
    <div className="flex items-center justify-between text-xs">
      <span className="text-muted-foreground">{label}</span>
      <span className="tabular-nums">{value}</span>
    </div>
  );
  return (
    <Context usedTokens={used} maxTokens={max} usage={{ inputTokens: usage.input, outputTokens: usage.output, totalTokens: usage.input + usage.output, cachedInputTokens: usage.cached } as never} modelId={model}>
      <ContextTrigger className="h-6 gap-1 px-1.5 text-xs" />
      <ContextContent>
        <ContextContentHeader />
        <ContextContentBody className="space-y-1">
          {row("Input", compact(usage.input))}
          {usage.cached > 0 && row("From cache", compact(usage.cached))}
          {row("Output", compact(usage.output))}
          {row("Time", `${(usage.ms / 1000).toFixed(1)}s`)}
        </ContextContentBody>
        <ContextContentFooter>
          <span className="text-muted-foreground">Cost</span>
          <span className="tabular-nums">{usage.cost > 0 ? `${usage.currency}${usage.cost < 0.01 ? usage.cost.toFixed(4) : usage.cost.toFixed(2)}` : "free"}</span>
        </ContextContentFooter>
      </ContextContent>
    </Context>
  );
}

// ---- starter prompts

/** On an empty chat: a few things to ask, from what this person has connected. */
export function StarterSuggestions({ admin }: { admin: boolean }) {
  const { say, status, call, ready } = useLyra();
  // Their saved prompts (theirs first, then shared) as starters too: a tap puts one in the box to finish.
  const [saved, setSaved] = useState<{ title: string; prompt: string }[]>([]);
  useEffect(() => {
    if (!ready) return;
    void call<{ mine?: { title: string; prompt: string }[]; shared?: { title: string; prompt: string }[] }>("templates").then((t) => setSaved([...(t?.mine ?? []), ...(t?.shared ?? [])].slice(0, 3)));
  }, [ready, call]);
  const list = [
    "Plan my day",
    "What's on my calendar today?",
    "Anything in my inbox that needs me?",
    ...(status.pmi ? ["What are my tasks due this week?"] : []),
    "Show my notes and lists",
    "What do you remember about me?",
    ...(admin ? ["How are my machines doing?"] : []),
  ];
  // Wrapped and centred (the scrolling row cut the last ones off).
  return (
    <div className="flex max-w-xl flex-wrap justify-center gap-2">
      {list.map((q) => (
        <Suggestion key={q} suggestion={q} onClick={(text) => say(text)} />
      ))}
      {saved.map((t) => (
        <Suggestion key={`saved-${t.title}`} suggestion={t.title} className="border-teal-700/50" onClick={() => window.dispatchEvent(new CustomEvent("lyra-prefill", { detail: t.prompt }))} />
      ))}
    </div>
  );
}
