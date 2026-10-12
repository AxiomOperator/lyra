// Agents: the specialists lyra hands work to, for admins. Each has its own
// colour and icon (as on its cards in the chat), can be shared with members
// or kept to admins (the Operator and the Coder always are), duplicated as a
// starting point for a new one, and turned off. Changes go through /agent.
// New agent: a short form (a template or from scratch); lyra drafts its
// instructions and tries it on a test task, then it's activated or discarded.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { Archive, BookOpen, Bot, Briefcase, CalendarDays, ChartLine, Code2, Copy, Loader2, PenLine, Plus, Search, Server, Shield, Sparkles, type LucideIcon } from "lucide-react";
import { useState } from "react";
import { Back, Page, useAction } from "./parts";
import { useLyra } from "./store";

export const AGENT_ICONS: Record<string, LucideIcon> = {
  bot: Bot,
  pen: PenLine,
  search: Search,
  archive: Archive,
  server: Server,
  code: Code2,
  briefcase: Briefcase,
  calendar: CalendarDays,
  chart: ChartLine,
  shield: Shield,
  book: BookOpen,
  sparkles: Sparkles,
};

/** Each colour's classes (written out, so the styles are built in). */
export const AGENT_COLORS: Record<string, { card: string; icon: string; text: string; dot: string }> = {
  sky: { card: "border-sky-900/60 bg-sky-950/30", icon: "text-sky-400", text: "text-sky-200", dot: "bg-sky-500" },
  teal: { card: "border-teal-900/60 bg-teal-950/30", icon: "text-teal-400", text: "text-teal-200", dot: "bg-teal-500" },
  emerald: { card: "border-emerald-900/60 bg-emerald-950/30", icon: "text-emerald-400", text: "text-emerald-200", dot: "bg-emerald-500" },
  amber: { card: "border-amber-900/60 bg-amber-950/30", icon: "text-amber-400", text: "text-amber-200", dot: "bg-amber-500" },
  orange: { card: "border-orange-900/60 bg-orange-950/30", icon: "text-orange-400", text: "text-orange-200", dot: "bg-orange-500" },
  rose: { card: "border-rose-900/60 bg-rose-950/30", icon: "text-rose-400", text: "text-rose-200", dot: "bg-rose-500" },
  violet: { card: "border-violet-900/60 bg-violet-950/30", icon: "text-violet-400", text: "text-violet-200", dot: "bg-violet-500" },
  fuchsia: { card: "border-fuchsia-900/60 bg-fuchsia-950/30", icon: "text-fuchsia-400", text: "text-fuchsia-200", dot: "bg-fuchsia-500" },
  slate: { card: "border-slate-700/60 bg-slate-900/40", icon: "text-slate-300", text: "text-slate-200", dot: "bg-slate-400" },
};

export interface AgentRow {
  name: string;
  title: string;
  description: string;
  working: boolean;
  enabled: boolean;
  color: string;
  icon: string;
  shared: boolean;
  admin_only: boolean;
  delegations: number;
}

/** On/off, as a switch (like the Settings page's). */
function Switch({ checked, onCheckedChange, disabled, ...rest }: { checked: boolean; onCheckedChange: (on: boolean) => void; disabled?: boolean; "aria-label": string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={rest["aria-label"]}
      disabled={disabled}
      onClick={() => onCheckedChange(!checked)}
      className={cn("relative inline-flex h-6 w-11 shrink-0 items-center rounded-full border transition-colors disabled:opacity-50", checked ? "border-teal-600 bg-teal-600" : "border-muted-foreground/40 bg-muted")}
    >
      <span className={cn("inline-block size-4 rounded-full shadow transition-transform", checked ? "translate-x-6 bg-white" : "translate-x-1 bg-muted-foreground")} />
    </button>
  );
}

/** How an agent looks: its colour's classes and its icon. */
export function agentLook(a?: { color?: string; icon?: string }) {
  return { colors: AGENT_COLORS[a?.color ?? ""] ?? AGENT_COLORS.sky, Icon: AGENT_ICONS[a?.icon ?? ""] ?? Bot };
}

function AgentCard({ a, act, busy }: { a: AgentRow; act: (c: string) => Promise<boolean>; busy: boolean }) {
  const { colors, Icon } = agentLook(a);
  const [copy, setCopy] = useState<string | null>(null);
  return (
    <Card className={cn("gap-2 py-3", colors.card, !a.enabled && "opacity-60")}>
      <CardHeader className="px-4">
        <CardTitle className={cn("flex items-center gap-2 text-sm", colors.text)}>
          <Icon className={cn("size-4", colors.icon)} /> {a.title}
          {!a.enabled && <Badge variant="outline">off</Badge>}
          {a.working && <Badge variant="secondary">working</Badge>}
        </CardTitle>
        <CardDescription>{a.description}</CardDescription>
        <CardAction className="text-muted-foreground text-xs">{a.delegations ? `${a.delegations}×` : ""}</CardAction>
      </CardHeader>
      <CardContent className="space-y-3 px-4">
        <div className="flex flex-wrap items-center gap-1.5" aria-label="Colour">
          {Object.entries(AGENT_COLORS).map(([c, cls]) => (
            <button
              key={c}
              type="button"
              title={c}
              aria-label={`Colour ${c}`}
              aria-pressed={a.color === c}
              disabled={busy}
              onClick={() => void act(`/agent look ${a.name} ${c}`)}
              className={cn("size-5 rounded-full ring-offset-2 ring-offset-background", cls.dot, a.color === c && "ring-2 ring-foreground")}
            />
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-1" aria-label="Icon">
          {Object.entries(AGENT_ICONS).map(([name, I]) => (
            <Button key={name} size="icon" variant={a.icon === name ? "secondary" : "ghost"} className="size-7" title={name} aria-label={`Icon ${name}`} disabled={busy} onClick={() => void act(`/agent look ${a.name} - ${name}`)}>
              <I className="size-4" />
            </Button>
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-sm">
          <label className="flex items-center gap-2" title={a.admin_only ? "Works on machines and code: always admins' only" : "Members may use it in their conversations"}>
            <Switch checked={a.shared} disabled={busy || a.admin_only} onCheckedChange={(on) => void act(`/agent share ${a.name} ${on ? "on" : "off"}`)} aria-label="Shared with members" />
            Members may use it
          </label>
          <label className="flex items-center gap-2">
            <Switch checked={a.enabled} disabled={busy} onCheckedChange={(on) => void act(`/agent ${on ? "enable" : "disable"} ${a.name}`)} aria-label="On" />
            On
          </label>
          {copy === null ? (
            <Button size="sm" variant="ghost" className="ml-auto" onClick={() => setCopy(`${a.title} copy`)}>
              <Copy /> Duplicate
            </Button>
          ) : (
            <form
              className="ml-auto flex items-center gap-1"
              onSubmit={async (e) => {
                e.preventDefault();
                if (copy.trim() && (await act(`/agent duplicate ${a.name} ${copy.trim()}`))) setCopy(null);
              }}
            >
              <Input autoFocus value={copy} onChange={(e) => setCopy(e.target.value)} className="h-8 w-44" aria-label="The copy's name" />
              <Button size="sm" type="submit" disabled={busy || !copy.trim()}>
                Make copy
              </Button>
              <Button size="sm" variant="ghost" type="button" onClick={() => setCopy(null)}>
                Cancel
              </Button>
            </form>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

// ---- a new agent

const TEMPLATES: { id: string; label: string }[] = [
  { id: "custom", label: "From scratch" },
  { id: "writer", label: "Writer" },
  { id: "researcher", label: "Researcher" },
  { id: "assistant", label: "Assistant" },
  { id: "analyst", label: "Analyst" },
  { id: "data-analyst", label: "Data analyst" },
  { id: "project-manager", label: "Project manager" },
  { id: "reviewer", label: "Reviewer" },
  { id: "developer", label: "Developer" },
  { id: "archivist", label: "Archivist" },
];
const TONES = ["Professional", "Concise", "Friendly", "Formal", "Technical", "Adaptive to context"];
const MEMORY = [
  { id: "read", label: "Shared memory, read-only" },
  { id: "write", label: "Shared memory, read and write" },
  { id: "preferences", label: "Your preferences only" },
  { id: "none", label: "Nothing" },
];
const TOOLS = [
  { id: "none", label: "None" },
  { id: "read", label: "Read memory" },
  { id: "write", label: "Read and write memory" },
];

interface Built {
  id: string;
  title: string;
  name: string;
  summary: string;
  test?: string;
  notes?: string[];
  error?: string;
}

const field = "rounded-md border bg-background px-2 py-1.5 text-sm";

function NewAgent({ done }: { done: (text: string) => void }) {
  const { call } = useLyra();
  const [form, setForm] = useState({ template: "custom", name: "", purpose: "", tone: [] as string[], behavior: "", tools: "none", memory: "read", auto: false, model: "" });
  const [working, setWorking] = useState(false);
  const [built, setBuilt] = useState<Built | null>(null);
  const [error, setError] = useState("");
  const set = (change: Partial<typeof form>) => setForm((f) => ({ ...f, ...change }));
  const custom = form.template === "custom";
  const build = async () => {
    setWorking(true);
    setError("");
    const r = await call<Built>("agent_draft", form);
    setWorking(false);
    if (r.error) setError(r.error);
    else setBuilt(r);
  };
  const finish = async (what: "agent_activate" | "agent_discard") => {
    if (!built) return;
    setWorking(true);
    const r = await call<{ ok?: boolean; text?: string; error?: string }>(what, { id: built.id });
    setWorking(false);
    if (r.error) return setError(r.error);
    done(r.text ?? "");
  };
  return (
    <Card className="gap-3 py-4">
      <CardHeader className="px-4">
        <CardTitle className="flex items-center gap-2 text-sm">
          <Plus className="size-4" /> New agent
        </CardTitle>
        <CardDescription>lyra writes its instructions from this and tries it on a test task. You see how it did before it's added.</CardDescription>
      </CardHeader>
      <CardContent className="space-y-3 px-4 text-sm">
        {built ? (
          <>
            <div className="max-h-96 overflow-y-auto whitespace-pre-wrap break-words rounded-md border bg-muted/30 p-3 text-xs">{built.summary}</div>
            {built.test && <div className="max-h-72 overflow-y-auto whitespace-pre-wrap break-words rounded-md border bg-muted/30 p-3 text-xs">{built.test}</div>}
            {built.notes?.map((n) => (
              <p key={n} className="text-amber-300 text-xs">
                {n}
              </p>
            ))}
            <div className="flex flex-wrap gap-2">
              <Button size="sm" disabled={working} onClick={() => void finish("agent_activate")}>
                Activate {built.title}
              </Button>
              <Button size="sm" variant="secondary" disabled={working} onClick={() => setBuilt(null)}>
                Change it
              </Button>
              <Button size="sm" variant="ghost" disabled={working} onClick={() => void finish("agent_discard")}>
                Discard
              </Button>
            </div>
          </>
        ) : (
          <form
            className="space-y-3"
            onSubmit={(e) => {
              e.preventDefault();
              void build();
            }}
          >
            <div className="grid gap-3 sm:grid-cols-2">
              <label className="space-y-1">
                <span className="text-muted-foreground text-xs">Start from</span>
                <select value={form.template} onChange={(e) => set({ template: e.target.value })} className={cn(field, "w-full")} aria-label="Start from">
                  {TEMPLATES.map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="space-y-1">
                <span className="text-muted-foreground text-xs">Name{custom ? "" : " (optional: the template's)"}</span>
                <Input value={form.name} onChange={(e) => set({ name: e.target.value })} placeholder={custom ? "e.g. Support" : TEMPLATES.find((t) => t.id === form.template)?.label} aria-label="Name" />
              </label>
            </div>
            <label className="block space-y-1">
              <span className="text-muted-foreground text-xs">What it handles{custom ? "" : " (optional)"}</span>
              <Input value={form.purpose} onChange={(e) => set({ purpose: e.target.value })} placeholder="e.g. replies to customer emails" aria-label="What it handles" />
            </label>
            <div className="space-y-1">
              <span className="text-muted-foreground text-xs">How it works and writes</span>
              <div className="flex flex-wrap gap-1.5">
                {TONES.map((t) => {
                  const on = form.tone.includes(t);
                  return (
                    <Button key={t} type="button" size="sm" variant={on ? "secondary" : "outline"} className="h-7" aria-pressed={on} onClick={() => set({ tone: on ? form.tone.filter((x) => x !== t) : [...form.tone, t] })}>
                      {t}
                    </Button>
                  );
                })}
              </div>
            </div>
            <label className="block space-y-1">
              <span className="text-muted-foreground text-xs">Anything it should always or never do (optional)</span>
              <Textarea value={form.behavior} onChange={(e) => set({ behavior: e.target.value })} rows={2} placeholder="e.g. never promise refunds; always end with next steps" aria-label="Always or never" />
            </label>
            <div className="grid gap-3 sm:grid-cols-3">
              {custom && (
                <label className="space-y-1">
                  <span className="text-muted-foreground text-xs">Tools</span>
                  <select value={form.tools} onChange={(e) => set({ tools: e.target.value })} className={cn(field, "w-full")} aria-label="Tools">
                    {TOOLS.map((t) => (
                      <option key={t.id} value={t.id}>
                        {t.label}
                      </option>
                    ))}
                  </select>
                </label>
              )}
              <label className="space-y-1">
                <span className="text-muted-foreground text-xs">Memory</span>
                <select value={form.memory} onChange={(e) => set({ memory: e.target.value })} className={cn(field, "w-full")} aria-label="Memory">
                  {MEMORY.map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.label}
                    </option>
                  ))}
                </select>
              </label>
              {custom && (
                <label className="space-y-1">
                  <span className="text-muted-foreground text-xs">Model (optional)</span>
                  <Input value={form.model} onChange={(e) => set({ model: e.target.value })} placeholder="the main agent's" aria-label="Model" />
                </label>
              )}
            </div>
            <label className="flex items-center gap-2">
              <Switch checked={form.auto} onCheckedChange={(on) => set({ auto: on })} aria-label="Takes matching requests on its own" />
              lyra hands it matching requests on its own (otherwise only when asked, or @name)
            </label>
            {error && <p className="text-red-300 text-xs">{error}</p>}
            <div className="flex items-center gap-2">
              <Button size="sm" type="submit" disabled={working || (custom && (!form.name.trim() || !form.purpose.trim()))}>
                {working ? <Loader2 className="animate-spin" /> : <Sparkles />} {working ? "Drafting and trying it…" : "Build and try it"}
              </Button>
              <Button size="sm" variant="ghost" type="button" disabled={working} onClick={() => done("")}>
                Cancel
              </Button>
              {working && <span className="text-muted-foreground text-xs">This can take a minute or two.</span>}
            </div>
          </form>
        )}
        {built && error && <p className="text-red-300 text-xs">{error}</p>}
      </CardContent>
    </Card>
  );
}

export function AgentsPage({ onBack }: { onBack: () => void }) {
  const { status } = useLyra();
  const { act, busy, note } = useAction(() => {});
  const [adding, setAdding] = useState(false);
  const [added, setAdded] = useState("");
  const agents = (status.agents ?? []) as AgentRow[];
  return (
    <Page
      title="Agents"
      description="The specialists lyra hands work to: new ones, how each looks, whether members may use it, and copies to start from."
      action={
        <div className="flex items-center gap-2">
          {!adding && (
            <Button size="sm" onClick={() => setAdding(true)}>
              <Plus /> New agent
            </Button>
          )}
          <Back onBack={onBack} />
        </div>
      }
    >
      {note}
      {added && <p className="rounded-md border border-teal-700/60 bg-teal-950/30 px-3 py-2 text-sm text-teal-100">{added}</p>}
      {adding && (
        <NewAgent
          done={(text) => {
            setAdding(false);
            setAdded(text);
          }}
        />
      )}
      {agents.length === 0 && !adding && <p className="text-muted-foreground text-sm">No agents yet: New agent makes one.</p>}
      <div className="grid gap-3 md:grid-cols-2">
        {agents.map((a) => (
          <AgentCard key={a.name} a={a} act={act} busy={busy} />
        ))}
      </div>
    </Page>
  );
}
