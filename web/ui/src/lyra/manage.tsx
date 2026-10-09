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
import { Archive, Check, ChevronDown, Pause, Pencil, Play, Plus, RefreshCw, Search, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent, type ReactNode } from "react";
import { ago } from "./push";
import { Back, Failed, Page, useAction, useConfirm } from "./parts";
import { useLyra } from "./store";
import type { GoalRow, GoalsPageData, MemoryPageData, ModelsData, Routine, RoutineRun, RulesData, SkillRow, SkillsPageData, SystemRules } from "./types";

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
          {s.mine && <Badge className="bg-primary/15 text-primary">yours</Badge>}
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
        {/* Their own to decide; a shared one is an admin's. */}
        {s.can_decide === false ? (
          s.status === "proposed" && <p className="text-muted-foreground text-xs">Shared: an admin approves it.</p>
        ) : (
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
        )}
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

// ---- routines

const examples = ["every day at 07:00", "weekdays at 8:30", "monday at 9:00", "every 6h"];

/** "in 3 h" / "in 12 min" */
function until(iso: string) {
  const ms = new Date(iso).getTime() - Date.now();
  if (ms <= 0) return "now";
  const m = Math.round(ms / 60000);
  return m < 60 ? `in ${m} min` : m < 48 * 60 ? `in ${Math.round(m / 60)} h` : `in ${Math.round(m / 1440)} d`;
}

/** A routine's settings, edited in a dialog (new when `routine` is null). */
function RoutineDialog({ open, routine, onClose, act }: { open: boolean; routine: Routine | null; onClose: () => void; act: (c: string) => Promise<boolean> }) {
  const [name, setName] = useState("");
  const [schedule, setSchedule] = useState("");
  const [prompt, setPrompt] = useState("");
  const [notify, setNotify] = useState<Routine["notify"]>("problems");
  const [changes, setChanges] = useState(false);
  const [email, setEmail] = useState(false);
  useEffect(() => {
    setChanges(routine?.changes ?? false);
    setEmail(routine?.email ?? false);
    setName(routine?.name ?? "");
    setSchedule(routine?.schedule ?? "every day at 07:00");
    setPrompt(routine?.prompt ?? "");
    setNotify(routine?.notify ?? "problems");
  }, [routine, open]);
  // One line each: the command separates fields with "|".
  const clean = (t: string) => t.replace(/\s+/g, " ").replace(/\|/g, "/").trim();
  const save = async () => {
    let ok: boolean;
    if (!routine) {
      ok = await act(`/routine new ${clean(name)} | ${clean(schedule)} | ${clean(prompt)} | notify ${notify}${changes ? " | changes" : ""}${email ? " | email" : ""}`);
    } else {
      ok = true;
      if (clean(schedule) !== routine.schedule) ok = (await act(`/routine edit ${routine.name} schedule ${clean(schedule)}`)) && ok;
      if (ok && clean(prompt) !== routine.prompt) ok = (await act(`/routine edit ${routine.name} prompt ${clean(prompt)}`)) && ok;
      if (ok && notify !== routine.notify) ok = (await act(`/routine notify ${routine.name} ${notify}`)) && ok;
      if (ok && changes !== routine.changes) ok = (await act(`/routine edit ${routine.name} changes ${changes ? "on" : "off"}`)) && ok;
      if (ok && email !== !!routine.email) ok = (await act(`/routine edit ${routine.name} email ${email ? "on" : "off"}`)) && ok;
    }
    if (ok) onClose();
  };
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{routine ? `Edit ${routine.name}` : "New routine"}</DialogTitle>
          <DialogDescription>Something lyra does on a schedule, by itself. You hear about it only when it matters.</DialogDescription>
        </DialogHeader>
        <div className="space-y-3">
          {!routine && <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Name, e.g. morning-check" />}
          <div className="space-y-1.5">
            <Input value={schedule} onChange={(e) => setSchedule(e.target.value)} placeholder="When" />
            <div className="flex flex-wrap gap-1.5">
              {examples.map((x) => (
                <button key={x} type="button" onClick={() => setSchedule(x)} className="rounded-full border px-2 py-0.5 text-muted-foreground text-xs hover:bg-accent/50">
                  {x}
                </button>
              ))}
            </div>
          </div>
          <Textarea value={prompt} onChange={(e) => setPrompt(e.target.value)} rows={4} placeholder="What to do, e.g. check disk space, pending updates and failed services on @all" />
          <label className="flex items-start gap-2 text-sm">
            <input type="checkbox" className="mt-0.5 size-4 accent-teal-500" checked={changes} onChange={(e) => setChanges(e.target.checked)} />
            <span>
              It may change things
              <span className="block text-muted-foreground text-xs">Each change still asks you. Off: it only checks and reports, so it never waits on you.</span>
            </span>
          </label>
          <label className="flex items-start gap-2 text-sm">
            <input type="checkbox" className="mt-0.5 size-4 accent-teal-500" checked={email} onChange={(e) => setEmail(e.target.checked)} />
            <span>
              Email me each result
              <span className="block text-muted-foreground text-xs">To you only, e.g. a morning digest with links. lyra writes the answer as the email.</span>
            </span>
          </label>
          <label className="flex items-center justify-between gap-3 text-sm">
            <span>Tell me</span>
            <select value={notify} onChange={(e) => setNotify(e.target.value as Routine["notify"])} className="rounded-md border bg-background px-2 py-1.5 text-sm">
              <option value="problems">only when something needs me</option>
              <option value="always">after every run</option>
              <option value="never">never (Activity only)</option>
            </select>
          </label>
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button disabled={!clean(prompt) || !clean(schedule) || (!routine && !clean(name))} onClick={save}>
            {routine ? "Save" : "Create"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function RunLine({ run, open }: { run: RoutineRun; open: (session: string) => void }) {
  const [more, setMore] = useState(false);
  const mark = run.outcome !== "ok" ? `✗ ${run.outcome}` : run.needs_user ? "⚠ needs you" : "✓ all clear";
  return (
    <div className="rounded-md border px-3 py-2 text-sm">
      <div className="flex items-center justify-between gap-2">
        <span className={cn("font-medium", run.outcome !== "ok" || run.needs_user ? "text-red-300" : "text-emerald-300")}>{mark}</span>
        <span className="text-muted-foreground text-xs">
          {ago(run.at)} · {run.seconds}s · {run.decided_by}
        </span>
      </div>
      <p className={cn("mt-1 whitespace-pre-wrap break-words text-muted-foreground text-xs", !more && "line-clamp-3")}>{run.summary}</p>
      {run.emailed && <p className={cn("mt-1 text-xs", run.emailed.startsWith("not ") ? "text-red-300" : "text-teal-300")}>✉ {run.emailed}</p>}
      <div className="mt-1 flex gap-3 text-xs">
        <button type="button" className="text-muted-foreground hover:text-foreground" onClick={() => setMore(!more)}>
          {more ? "less" : "more"}
        </button>
        <button type="button" className="text-teal-300 hover:text-teal-200" onClick={() => open(run.session)}>
          open the conversation
        </button>
      </div>
    </div>
  );
}

export function RoutinesPage({ onBack, toChat }: { onBack: () => void; toChat: () => void }) {
  const { status, say } = useLyra();
  // Follows runs starting and finishing.
  const [data, reload] = usePage<Routine[] | { error: string }>("routines", JSON.stringify(status.routines ?? null));
  const { act, busy, note } = useAction(reload);
  const [confirm, dialog] = useConfirm();
  const [editing, setEditing] = useState<{ routine: Routine | null } | null>(null);
  const [history, setHistory] = useState<string | null>(null);
  const routines = Array.isArray(data) ? data : [];
  const open = (session: string) => {
    say(`/resume ${session}`);
    toChat();
  };
  return (
    <Page
      title="Routines"
      description="Things lyra does on a schedule — it tells you only when something needs you."
      action={
        <div className="flex gap-1">
          <Button size="sm" onClick={() => setEditing({ routine: null })}>
            <Plus /> New
          </Button>
          <Back onBack={onBack} />
        </div>
      }
    >
      {data && !Array.isArray(data) && <Failed error={data.error} />}
      {note}
      {data && routines.length === 0 && (
        <p className="text-muted-foreground text-sm">
          No routines yet. Add one here, or just ask lyra: "every morning at 7, check disk space and failed services on @all and tell me if anything's wrong".
        </p>
      )}
      {routines.map((r) => {
        const last = r.runs[0];
        return (
          <Card key={r.name} className={cn("gap-2 py-3", !r.enabled && "opacity-70")}>
            <CardHeader className="px-4">
              <CardTitle className="flex flex-wrap items-center gap-2 text-sm">
                {r.name}
                {r.running && <Badge className="bg-sky-600/80 text-white">running</Badge>}
                {!r.enabled && <Badge variant="secondary">paused</Badge>}
                {r.changes && <Badge variant="outline">may change things</Badge>}
                {r.email && <Badge variant="outline">emailed to you</Badge>}
                {!r.valid && <Badge className="bg-red-500/20 text-red-300">bad schedule</Badge>}
              </CardTitle>
              <CardDescription className="break-words">
                {r.schedule}
                {r.next && ` · next ${until(r.next)} (${new Date(r.next).toLocaleString([], { weekday: "short", hour: "2-digit", minute: "2-digit" })})`}
                {` · tells you ${r.notify === "problems" ? "when something needs you" : r.notify === "always" ? "after every run" : "never"}`}
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-2 px-4">
              <p className="whitespace-pre-wrap break-words text-sm">{r.prompt}</p>
              {last ? <RunLine run={last} open={open} /> : <p className="text-muted-foreground text-xs">Not run yet.</p>}
              {history === r.name && r.runs.slice(1).map((x) => <RunLine key={x.at} run={x} open={open} />)}
              <div className="flex flex-wrap gap-2">
                <Button size="sm" variant="secondary" disabled={busy || r.running} onClick={() => act(`/routine run ${r.name}`)}>
                  <Play /> Run now
                </Button>
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => act(`/routine ${r.enabled ? "pause" : "resume"} ${r.name}`)}>
                  {r.enabled ? <Pause /> : <Play />} {r.enabled ? "Pause" : "Resume"}
                </Button>
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => setEditing({ routine: r })}>
                  <Pencil /> Edit
                </Button>
                {r.runs.length > 1 && (
                  <Button size="sm" variant="ghost" onClick={() => setHistory(history === r.name ? null : r.name)}>
                    <ChevronDown className={cn(history === r.name && "rotate-180")} /> {r.runs.length - 1} earlier
                  </Button>
                )}
                <Button
                  size="sm"
                  variant="ghost"
                  className="text-red-400 hover:text-red-300"
                  disabled={busy}
                  onClick={() => confirm({ title: `Delete ${r.name}?`, text: "It stops running; its past runs stay in your conversations.", action: "Delete", run: () => void act(`/routine delete ${r.name}`) })}
                >
                  <Trash2 /> Delete
                </Button>
              </div>
            </CardContent>
          </Card>
        );
      })}
      <RoutineDialog open={!!editing} routine={editing?.routine ?? null} onClose={() => setEditing(null)} act={act} />
      {dialog}
    </Page>
  );
}
