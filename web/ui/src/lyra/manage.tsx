// Managing lyra from the app: memory, skills, goals, the model, and a
// machine's rules. Pages ask for their data with `call` and change things
// with lyra's own commands (`run`), so the app does what the terminal does.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { Archive, ArrowLeft, Check, ChevronDown, Pause, Pencil, Play, Plus, RefreshCw, Search, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import { ago } from "./push";
import { Page, useConfirm } from "./parts";
import { useLyra } from "./store";
import type { GoalRow, GoalsPageData, MemoryPageData, ModelsData, RulesData, SkillRow, SkillsPageData, SystemRules } from "./types";

/** Load a page's data, again after every change. */
function usePage<T>(what: string, arg?: unknown) {
  const { call, ready } = useLyra();
  const [data, setData] = useState<T | null>(null);
  const key = JSON.stringify(arg ?? null);
  const load = useCallback(() => {
    void call<T>(what, arg).then(setData);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [call, what, key]);
  useEffect(() => {
    if (ready) load();
  }, [ready, load]);
  return [data, load] as const;
}

/** Run a command, show lyra's answer, reload the page. */
function useAction(reload: () => void) {
  const { run } = useLyra();
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const act = useCallback(
    async (command: string) => {
      setBusy(true);
      const r = await run(command);
      setBusy(false);
      setNote(r);
      reload();
      return r.ok;
    },
    [run, reload],
  );
  const shown = note && (
    <div className={cn("flex items-start justify-between gap-2 rounded-md border px-3 py-2 text-sm", note.ok ? "border-teal-700/60 bg-teal-950/30 text-teal-100" : "border-red-800/60 bg-red-950/30 text-red-200")}>
      <span className="whitespace-pre-wrap break-words">{note.text}</span>
      <button type="button" onClick={() => setNote(null)} className="text-muted-foreground hover:text-foreground">
        <X className="size-4" />
      </button>
    </div>
  );
  return { act, busy, note: shown };
}

function Back({ onBack }: { onBack: () => void }) {
  return (
    // Wide screens have the sidebar instead.
    <Button size="icon" variant="ghost" onClick={onBack} aria-label="Back" className="md:hidden">
      <ArrowLeft />
    </Button>
  );
}

function Failed({ error }: { error?: string }) {
  return error ? <p className="rounded-md border border-red-800/60 bg-red-950/30 px-3 py-2 text-red-200 text-sm">{error}</p> : null;
}

function Pct({ value }: { value: number }) {
  return <span className="text-muted-foreground text-xs tabular-nums">{Math.round(value * 100)}%</span>;
}

/** A text the user edits in a dialog (a memory's content). */
function EditDialog({ open, title, text, onSave, onClose }: { open: boolean; title: string; text: string; onSave: (text: string) => void; onClose: () => void }) {
  const [value, setValue] = useState(text);
  useEffect(() => setValue(text), [text, open]);
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>The old version is kept in the memory's history.</DialogDescription>
        </DialogHeader>
        <Textarea value={value} onChange={(e) => setValue(e.target.value)} rows={5} />
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button disabled={!value.trim() || value.trim() === text.trim()} onClick={() => onSave(value.trim().replace(/\s*\n\s*/g, " "))}>
            Save
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

// ---- memory

export function MemoryPage({ onBack }: { onBack: () => void }) {
  const [typed, setTyped] = useState("");
  const [query, setQuery] = useState("");
  const [scope, setScope] = useState("");
  const [data, reload] = usePage<MemoryPageData>("memory", { query, scope });
  const { act, busy, note } = useAction(reload);
  const [confirm, dialog] = useConfirm();
  const [editing, setEditing] = useState<{ id: string; content: string } | null>(null);
  const search = (e: FormEvent) => {
    e.preventDefault();
    setQuery(typed.trim());
  };
  return (
    <Page title="Memory" description={data ? `${data.active} active memories. Search, fix or drop what lyra remembers.` : "What lyra remembers."} action={<Back onBack={onBack} />}>
      <Failed error={data?.error} />
      {note}
      <form onSubmit={search} className="flex gap-2">
        <Input value={typed} onChange={(e) => setTyped(e.target.value)} placeholder="Search memories…" enterKeyHint="search" />
        <Button type="submit" size="icon" variant="secondary" aria-label="Search">
          <Search />
        </Button>
      </form>
      {!!data?.scopes.length && (
        <div className="flex flex-wrap gap-1.5">
          {[{ scope: "", count: data.active }, ...data.scopes].map((s) => (
            <button
              key={s.scope || "all"}
              type="button"
              onClick={() => setScope(s.scope)}
              className={cn("rounded-full border px-2.5 py-0.5 text-xs", scope === s.scope ? "border-teal-500 bg-teal-500/15 text-teal-200" : "text-muted-foreground hover:bg-accent/50")}
            >
              {s.scope || "all"} <span className="opacity-60">{s.count}</span>
            </button>
          ))}
        </div>
      )}
      {!!data?.proposals.length && (
        <Card className="border-amber-700/50 py-4">
          <CardHeader className="px-4">
            <CardTitle className="text-base">Waiting for you</CardTitle>
            <CardDescription>Changes the memory curator suggests.</CardDescription>
          </CardHeader>
          <CardContent className="space-y-2 px-4">
            {data.proposals.map((p) => (
              <div key={p.id} className="flex items-start justify-between gap-2 rounded-md border px-3 py-2">
                <span className="min-w-0 break-words text-sm">{p.text.replace(/^\S+\s/, "")}</span>
                <span className="flex shrink-0 gap-1">
                  <Button size="icon" variant="secondary" disabled={busy} onClick={() => act(`/memory approve ${p.id}`)} aria-label="Approve">
                    <Check />
                  </Button>
                  <Button size="icon" variant="ghost" disabled={busy} onClick={() => act(`/memory reject ${p.id}`)} aria-label="Reject">
                    <X />
                  </Button>
                </span>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
      {data && !data.error && data.memories.length === 0 && <p className="text-muted-foreground text-sm">{query ? "Nothing found." : "No memories yet."}</p>}
      {data?.memories.map((m) => (
        <Card key={m.id} className="gap-2 py-3">
          <CardContent className="space-y-2 px-4">
            <p className="whitespace-pre-wrap break-words text-sm">{m.content}</p>
            <div className="flex flex-wrap items-center gap-1.5">
              <Badge variant="secondary">{m.kind}</Badge>
              <Badge variant="outline">{m.scope}</Badge>
              <span className="text-muted-foreground text-xs">
                sure {Math.round(m.confidence * 100)}% · {ago(m.updated)}
                {m.score !== null && ` · match ${m.score.toFixed(2)}`}
              </span>
              <span className="ml-auto flex gap-0.5">
                <Button size="icon" variant="ghost" className="size-8" disabled={busy} onClick={() => setEditing({ id: m.id, content: m.content })} aria-label="Edit">
                  <Pencil />
                </Button>
                <Button size="icon" variant="ghost" className="size-8" disabled={busy} onClick={() => act(`/memory archive ${m.id}`)} aria-label="Archive">
                  <Archive />
                </Button>
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-8 text-red-400 hover:text-red-300"
                  disabled={busy}
                  aria-label="Forget"
                  onClick={() =>
                    confirm({
                      title: "Forget this memory?",
                      text: `"${m.content.slice(0, 160)}" — lyra stops using it. /memory restore ${m.id} brings it back.`,
                      action: "Forget",
                      run: () => void act(`/memory forget ${m.id}`),
                    })
                  }
                >
                  <Trash2 />
                </Button>
              </span>
            </div>
          </CardContent>
        </Card>
      ))}
      <EditDialog
        open={!!editing}
        title="Correct this memory"
        text={editing?.content ?? ""}
        onClose={() => setEditing(null)}
        onSave={(text) => {
          if (editing) void act(`/memory correct ${editing.id} ${text}`);
          setEditing(null);
        }}
      />
      {dialog}
    </Page>
  );
}

// ---- skills

const skillOrder: SkillRow["status"][] = ["proposed", "active", "deprecated", "rejected"];

function SkillCard({ s, busy, act }: { s: SkillRow; busy: boolean; act: (c: string) => void }) {
  const [open, setOpen] = useState(s.status === "proposed");
  return (
    <Card className="gap-2 py-3">
      <CardHeader className="px-4">
        <CardTitle className="flex flex-wrap items-center gap-2 text-sm">
          {s.name}
          {s.agent && <Badge variant="outline">{s.agent}</Badge>}
        </CardTitle>
        <CardDescription className="break-words">{s.description}</CardDescription>
        <CardAction>
          <button type="button" onClick={() => setOpen(!open)} className="text-muted-foreground" aria-label="Show instructions">
            <ChevronDown className={cn("size-4 transition-transform", open && "rotate-180")} />
          </button>
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-2 px-4">
        {open && <pre className="max-h-64 overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/40 p-2 font-mono text-xs">{s.instructions}</pre>}
        <p className="text-muted-foreground text-xs">{s.record}</p>
        <div className="flex flex-wrap gap-2">
          {s.status !== "active" && (
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => act(`/approve ${s.id}`)}>
              <Check /> {s.status === "proposed" ? "Approve" : "Use again"}
            </Button>
          )}
          {s.status === "proposed" && (
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => act(`/reject ${s.id}`)}>
              <X /> Reject
            </Button>
          )}
          {s.status === "active" && (
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => act(`/deprecate ${s.id}`)}>
              <Pause /> Stop using
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

export function SkillsPage({ onBack }: { onBack: () => void }) {
  const [data, reload] = usePage<SkillsPageData>("skills");
  const { act, busy, note } = useAction(reload);
  return (
    <Page title="Skills" description={data ? `What lyra has learned to do · learning ${data.mode}` : "What lyra has learned to do."} action={<Back onBack={onBack} />}>
      <Failed error={data?.error} />
      {note}
      {!!data?.proposals.length && (
        <Card className="border-amber-700/50 py-4">
          <CardHeader className="px-4">
            <CardTitle className="text-base">Changes to review</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2 px-4">
            {data.proposals.map((p) => (
              <div key={p.id} className="space-y-1.5 rounded-md border px-3 py-2">
                <div className="font-medium text-sm">{p.change}</div>
                <div className="text-muted-foreground text-xs">{p.reason}</div>
                {p.detail && <pre className="max-h-48 overflow-auto whitespace-pre-wrap rounded bg-muted/40 p-2 font-mono text-xs">{p.detail}</pre>}
                <div className="flex gap-2">
                  <Button size="sm" variant="secondary" disabled={busy} onClick={() => act(`/approve ${p.id}`)}>
                    <Check /> Apply
                  </Button>
                  <Button size="sm" variant="ghost" disabled={busy} onClick={() => act(`/reject ${p.id}`)}>
                    <X /> Discard
                  </Button>
                </div>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
      {data && !data.error && data.skills.length === 0 && <p className="text-muted-foreground text-sm">No skills yet. Lessons from corrections and multi-step work show up here.</p>}
      {skillOrder.map((status) => {
        const group = (data?.skills ?? []).filter((s) => s.status === status);
        if (!group.length) return null;
        return (
          <section key={status} className="space-y-2">
            <h2 className="font-medium text-muted-foreground text-sm capitalize">
              {status} <span className="opacity-60">{group.length}</span>
            </h2>
            {group.map((s) => (
              <SkillCard key={s.id} s={s} busy={busy} act={act} />
            ))}
          </section>
        );
      })}
    </Page>
  );
}

// ---- goals

const statusColor: Record<string, string> = {
  active: "bg-teal-600/80 text-white",
  paused: "bg-amber-500/20 text-amber-300",
  blocked: "bg-red-500/20 text-red-300",
  completed: "bg-emerald-600/30 text-emerald-200",
};

function GoalCard({ g, busy, act, confirm }: { g: GoalRow; busy: boolean; act: (c: string) => void; confirm: ReturnType<typeof useConfirm>[0] }) {
  const open = !["completed", "cancelled", "failed"].includes(g.status);
  return (
    <Card className={cn("gap-2 py-3", g.parent && "ml-4", !open && "opacity-70")}>
      <CardHeader className="px-4">
        <CardTitle className="text-sm">{g.title}</CardTitle>
        {g.description && <CardDescription className="break-words">{g.description}</CardDescription>}
        <CardAction>
          <Badge className={statusColor[g.status] ?? ""} variant={statusColor[g.status] ? "default" : "secondary"}>
            {g.status}
          </Badge>
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-2 px-4">
        <div className="flex items-center gap-2">
          <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-muted">
            <div className="h-full bg-teal-500" style={{ width: `${Math.round(g.progress * 100)}%` }} />
          </div>
          <Pct value={g.progress} />
        </div>
        {(g.blocked || g.due) && (
          <p className="text-muted-foreground text-xs">
            {g.due && `due ${new Date(g.due).toLocaleDateString()}`}
            {g.blocked && g.due && " · "}
            {g.blocked && <span className="text-red-300">blocked: {g.blocked}</span>}
          </p>
        )}
        {open && (
          <div className="flex flex-wrap items-center gap-2">
            <label className="flex items-center gap-1.5 text-muted-foreground text-xs">
              priority
              <select
                value={g.priority}
                disabled={busy}
                onChange={(e) => act(`/goal priority ${g.id} ${e.target.value}`)}
                className="rounded-md border bg-background px-1.5 py-1 text-foreground text-xs"
              >
                {Array.from({ length: 11 }, (_, i) => (
                  <option key={i} value={i}>
                    {i}
                  </option>
                ))}
              </select>
            </label>
            {g.status === "paused" || g.status === "proposed" ? (
              <Button size="sm" variant="secondary" disabled={busy} onClick={() => act(`/goal activate ${g.id}`)}>
                <Play /> {g.status === "paused" ? "Resume" : "Start"}
              </Button>
            ) : (
              <Button size="sm" variant="secondary" disabled={busy} onClick={() => act(`/goal pause ${g.id}`)}>
                <Pause /> Pause
              </Button>
            )}
            <Button size="sm" variant="ghost" disabled={busy} onClick={() => act(`/goal complete ${g.id}`)}>
              <Check /> Done
            </Button>
            <Button
              size="sm"
              variant="ghost"
              className="text-red-400 hover:text-red-300"
              disabled={busy}
              onClick={() => confirm({ title: `Cancel "${g.title}"?`, text: "lyra stops working toward it. It stays in the list as cancelled.", action: "Cancel goal", run: () => act(`/goal cancel ${g.id}`) })}
            >
              <X /> Cancel
            </Button>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

export function GoalsPage({ onBack }: { onBack: () => void }) {
  const [data, reload] = usePage<GoalsPageData>("goals");
  const { act, busy, note } = useAction(reload);
  const [confirm, dialog] = useConfirm();
  const [title, setTitle] = useState("");
  const [what, setWhat] = useState("");
  const create = async (e: FormEvent) => {
    e.preventDefault();
    const t = title.trim().replace(/\s+/g, " ");
    if (!t) return;
    const d = what.trim().replace(/\s+/g, " ");
    if (await act(`/goal new ${t}${d ? ` -- ${d}` : ""}`)) {
      setTitle("");
      setWhat("");
    }
  };
  return (
    <Page title="Goals" description={data ? `Long-term goals, most important first · autonomy ${data.mode}` : "Long-term goals."} action={<Back onBack={onBack} />}>
      <Failed error={data?.error} />
      {note}
      <Card className="py-4">
        <CardContent className="px-4">
          <form onSubmit={create} className="space-y-2">
            <Input value={title} onChange={(e) => setTitle(e.target.value)} placeholder="A new goal…" />
            {title.trim() && <Input value={what} onChange={(e) => setWhat(e.target.value)} placeholder="What done looks like (optional)" />}
            <Button type="submit" size="sm" disabled={busy || !title.trim()}>
              <Plus /> Add goal
            </Button>
          </form>
        </CardContent>
      </Card>
      {data && !data.error && data.goals.length === 0 && <p className="text-muted-foreground text-sm">No goals yet.</p>}
      {data?.goals.map((g) => (
        <GoalCard key={g.id} g={g} busy={busy} act={act} confirm={confirm} />
      ))}
      {dialog}
    </Page>
  );
}

// ---- model

export function ModelsPage({ onBack }: { onBack: () => void }) {
  const [data, reload] = usePage<ModelsData>("models");
  const { act, busy, note } = useAction(reload);
  return (
    <Page
      title="Model"
      description="The model lyra talks with. A switch applies to every conversation and is saved."
      action={
        <div className="flex">
          <Button size="icon" variant="ghost" onClick={reload} aria-label="Refresh">
            <RefreshCw />
          </Button>
          <Back onBack={onBack} />
        </div>
      }
    >
      {note}
      {data?.error && <p className="text-muted-foreground text-sm">{data.error}</p>}
      {data && (
        <Card className="py-2">
          <CardContent className="divide-y divide-border px-4">
            {(data.models.includes(data.current) ? data.models : [data.current, ...data.models]).map((m) => (
              <div key={m} className="flex items-center justify-between gap-3 py-2.5">
                <span className="min-w-0 break-all font-mono text-sm">{m}</span>
                {m === data.current ? (
                  <Badge className="bg-teal-600/80 text-white">in use</Badge>
                ) : (
                  <Button size="sm" variant="secondary" disabled={busy} onClick={() => act(`/model ${m}`)}>
                    Use
                  </Button>
                )}
              </div>
            ))}
          </CardContent>
        </Card>
      )}
    </Page>
  );
}

// ---- a machine's rules

function Lines({ label, hint, value, onChange }: { label: string; hint: string; value: string[]; onChange: (v: string[]) => void }) {
  const [text, setText] = useState(value.join("\n"));
  return (
    <label className="block space-y-1">
      <span className="font-medium text-sm">{label}</span>
      <span className="block text-muted-foreground text-xs">{hint}</span>
      <Textarea
        value={text}
        rows={Math.min(8, Math.max(2, value.length + 1))}
        className="font-mono text-xs"
        onChange={(e) => {
          setText(e.target.value);
          onChange(e.target.value.split("\n"));
        }}
      />
    </label>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <label className="flex items-center justify-between gap-3 text-sm">
      <span>{label}</span>
      {children}
    </label>
  );
}

/** What runs without asking on a machine (or the server), edited by the user. */
export function RulesDialog({ machine, onClose }: { machine: string | null; onClose: () => void }) {
  const { call } = useLyra();
  const [data, setData] = useState<RulesData | null>(null);
  const [rules, setRules] = useState<SystemRules | null>(null);
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  useEffect(() => {
    setData(null);
    setRules(null);
    setResult(null);
    if (!machine) return;
    void call<RulesData>("rules", { machine }).then((d) => {
      setData(d);
      setRules(d.system ?? null);
    });
  }, [machine, call]);
  const set = <K extends keyof SystemRules>(k: K, v: SystemRules[K]) => rules && setRules({ ...rules, [k]: v });
  const save = async () => {
    if (!rules || !machine) return;
    setBusy(true);
    const d = await call<RulesData>("set_rules", { machine, system: rules });
    setBusy(false);
    if (d.error) {
      setResult({ ok: false, text: d.error });
    } else {
      setResult({ ok: true, text: `Saved${d.path ? ` to ${d.path}` : ""} — in effect now.` });
      if (d.system) setRules(d.system);
    }
  };
  const num = (k: "timeout_seconds" | "approval_timeout_seconds") => (
    <Input type="number" min={1} className="w-24" value={rules?.[k] ?? 0} onChange={(e) => set(k, Math.max(0, Number(e.target.value) || 0))} />
  );
  return (
    <Dialog open={!!machine} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Rules on {machine}</DialogTitle>
          <DialogDescription>What lyra may do there without asking. {machine === "server" ? "Saved to config.toml." : "The machine checks and keeps them itself."}</DialogDescription>
        </DialogHeader>
        {!data && <p className="text-muted-foreground text-sm">Asking {machine}…</p>}
        <Failed error={data?.error} />
        {rules && (
          <div key={JSON.stringify(data)} className="space-y-4">
            <Field label="System access on">
              <input type="checkbox" className="size-4 accent-teal-500" checked={rules.enabled} onChange={(e) => set("enabled", e.target.checked)} />
            </Field>
            <Lines label="Commands that run without asking" hint="One per line: a command or its start, e.g. `git pull`. Read-only commands never ask." value={rules.allow_commands} onChange={(v) => set("allow_commands", v)} />
            <Lines label="Folders lyra may write in without asking" hint="One per line, e.g. ~/Projects." value={rules.write_roots} onChange={(v) => set("write_roots", v)} />
            <Lines label="Off limits" hint="Paths never read or written (keys, credentials)." value={rules.deny_paths} onChange={(v) => set("deny_paths", v)} />
            <Lines label="SSH hosts" hint="Hosts commands may run on over SSH." value={rules.ssh_hosts} onChange={(v) => set("ssh_hosts", v)} />
            <Field label="Longest a command may run (s)">{num("timeout_seconds")}</Field>
            <Field label="How long an approval waits (s)">{num("approval_timeout_seconds")}</Field>
          </div>
        )}
        {result && <p className={cn("text-sm", result.ok ? "text-teal-300" : "text-red-300")}>{result.text}</p>}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Close
          </Button>
          <Button disabled={!rules || busy} onClick={save}>
            Save
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
