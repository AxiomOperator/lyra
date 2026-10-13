// Tasks: lyra's own tasks, reminders and projects, for everyone. Your tasks
// (overdue, today, upcoming), those someone gave you to accept or decline,
// those you gave others, and projects with their health and status updates.
// Live: lyra serve sends the view again whenever tasks change. What you do
// here is your own doing, so it happens at once (in the chat, anything others
// see waits for your Allow).

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { AlarmClock, Check, Circle, CircleCheck, FolderKanban, Plus, Repeat, Send, Trash2, UserPlus, X } from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { InboxCard, TodayCard } from "./calendar";
import { takeIntent } from "./intent";
import { Back, Failed, Page, useAction } from "./parts";
import { ago } from "./push";
import { useLyra } from "./store";
import type { ProjectBrief, TaskBrief, TaskFull } from "./types";

const today = () => {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};

/** A dialog footer with more below it: in its place, not stuck to the bottom. */
const inline = "static mx-0 mb-0 rounded-none border-t-0 bg-transparent p-0";

function dueText(due: string | null) {
  if (!due) return null;
  const t = today();
  if (due < t) return { text: `overdue · ${due}`, cls: "text-amber-400" };
  if (due === t) return { text: "today", cls: "text-teal-300" };
  return { text: new Date(`${due}T12:00`).toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" }), cls: "text-muted-foreground" };
}

const HEALTH: Record<string, { text: string; cls: string }> = {
  on_track: { text: "on track", cls: "border-emerald-700/60 text-emerald-300" },
  at_risk: { text: "at risk", cls: "border-amber-700/60 text-amber-300" },
  off_track: { text: "off track", cls: "border-red-700/60 text-red-300" },
};

const field = "rounded-md border bg-background px-2 py-1.5 text-sm";
const REPEATS = ["", "daily", "weekdays", "weekly", "biweekly", "monthly", "yearly"];

/** An RFC 3339 time as a datetime-local input's value. */
function localInput(at: string | null) {
  if (!at) return "";
  const d = new Date(at);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

type People = { id: string; name: string }[];
type Answer = { ok?: boolean; text?: string; error?: string };

function TaskRow({ task, onDone, onSnooze, onOpen, busy }: { task: TaskBrief; onDone: () => void; onSnooze: () => void; onOpen: () => void; busy: boolean }) {
  const due = dueText(task.due);
  const done = task.status === "done";
  return (
    <div className="flex items-start gap-3 py-2">
      <button type="button" disabled={busy || done} onClick={onDone} aria-label={`Done: ${task.title}`} className="mt-0.5 text-muted-foreground hover:text-teal-300 disabled:opacity-60">
        {done ? <CircleCheck className="size-5 text-teal-400" /> : <Circle className="size-5" />}
      </button>
      <button type="button" onClick={onOpen} className="min-w-0 flex-1 text-left">
        <div className={cn("text-sm", done && "text-muted-foreground line-through")}>{task.title}</div>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-muted-foreground text-xs">
          {due && !done && <span className={due.cls}>{due.text}</span>}
          <span>{task.where}</span>
          {task.status === "in_progress" && <span>in progress</span>}
          {(task.priority === "high" || task.priority === "urgent") && <span className="text-amber-300">{task.priority}</span>}
          {task.checklist && <span>☑ {task.checklist}</span>}
          {task.repeat && (
            <span className="flex items-center gap-0.5">
              <Repeat className="size-3" /> {task.repeat}
            </span>
          )}
          {task.assignees?.map((a) => (
            <span key={a.name} className={a.state === "pending" ? "text-amber-300" : a.state === "declined" ? "line-through" : ""}>
              → {a.name}
              {a.state === "pending" ? " (asked)" : ""}
            </span>
          ))}
        </div>
      </button>
      {!done && (
        <Button size="icon" variant="ghost" disabled={busy} onClick={onSnooze} aria-label="Remind me in an hour" title="Remind me in an hour">
          <AlarmClock />
        </Button>
      )}
    </div>
  );
}

function ProjectRow({ p, onOpen }: { p: ProjectBrief; onOpen: () => void }) {
  const h = HEALTH[p.health] ?? HEALTH.on_track;
  const pct = p.tasks.total ? Math.round((p.tasks.done / p.tasks.total) * 100) : 0;
  return (
    <button type="button" onClick={onOpen} className="flex w-full items-center gap-3 py-2 text-left">
      <div className="min-w-0 flex-1">
        <div className="text-sm">
          {p.name}
          {p.status !== "active" && <span className="ml-2 text-muted-foreground text-xs">{p.status.replace("_", " ")}</span>}
        </div>
        <div className="text-muted-foreground text-xs">
          {p.tasks.open} open{p.tasks.overdue ? ` · ${p.tasks.overdue} overdue` : ""} · {p.members.length} member{p.members.length === 1 ? "" : "s"}
          {p.last_update ? ` · updated ${ago(p.last_update.at)}` : " · no status update yet"}
        </div>
        <div className="mt-1 h-1 overflow-hidden rounded bg-muted">
          <div className="h-full bg-teal-500/70" style={{ width: `${pct}%` }} />
        </div>
      </div>
      <Badge variant="outline" className={h.cls}>
        {h.text}
      </Badge>
    </button>
  );
}

/** People to pick from (everyone else who uses lyra). */
function PeoplePicker({ people, value, onChange, label }: { people: People; value: string[]; onChange: (v: string[]) => void; label: string }) {
  if (!people.length) return <p className="text-muted-foreground text-xs">Nobody else uses lyra yet.</p>;
  return (
    <div className="flex flex-wrap gap-1.5" aria-label={label}>
      {people.map((p) => {
        const on = value.includes(p.name);
        return (
          <Button key={p.id} type="button" size="sm" variant={on ? "secondary" : "outline"} className="h-7" aria-pressed={on} onClick={() => onChange(on ? value.filter((x) => x !== p.name) : [...value, p.name])}>
            {p.name}
          </Button>
        );
      })}
    </div>
  );
}

/** A new task, or one task in full: change it, its checklist, comments, history. */
function TaskDialog({ id, open, onClose, people, projects, said }: { id: string | null; open: boolean; onClose: () => void; people: People; projects: ProjectBrief[]; said: (t: string) => void }) {
  const { call } = useLyra();
  const [t, setT] = useState<TaskFull | null>(null);
  const [form, setForm] = useState({ title: "", notes: "", due: "", remind: "", priority: "normal", repeat: "", project: "", assignees: [] as string[], items: [] as { done: boolean; text: string }[] });
  const [item, setItem] = useState("");
  const [comment, setComment] = useState("");
  const [give, setGive] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const set = (c: Partial<typeof form>) => setForm((f) => ({ ...f, ...c }));
  const load = useCallback(
    (x: TaskFull) => {
      setT(x);
      setForm({ title: x.title, notes: x.notes, due: x.due ?? "", remind: localInput(x.reminder), priority: x.priority, repeat: x.repeat ?? "", project: projects.find((p) => p.id === x.project_id)?.name ?? "", assignees: [], items: x.items });
    },
    [projects],
  );
  useEffect(() => {
    if (!open) return;
    setError("");
    setComment("");
    if (!id) {
      setT(null);
      setForm({ title: "", notes: "", due: "", remind: "", priority: "normal", repeat: "", project: "", assignees: [], items: [] });
      return;
    }
    void call<TaskFull & { error?: string }>("task", { id }).then((r) => (r.error ? setError(r.error) : load(r)));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, id]);
  const run = async (what: string, arg: object, close = false) => {
    setBusy(true);
    const r = await call<Answer & { task?: TaskFull }>(what, arg);
    setBusy(false);
    if (r.error) return setError(r.error), false;
    if (r.task) load(r.task);
    if (r.text) said(r.text);
    if (close) onClose();
    return true;
  };
  const save = (e: FormEvent) => {
    e.preventDefault();
    const remind = form.remind.replace("T", " ");
    void run("task_save", { id: t?.id ?? "", title: form.title, notes: form.notes, due: form.due || "none", remind: t ? remind : remind, priority: form.priority, repeat: form.repeat || "none", project: form.project || "none", assignees: form.assignees, checklist: form.items }, true);
  };
  const readOnly = !!t && t.waiting_for_you;
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{t ? t.title : "New task"}</DialogTitle>
          <DialogDescription>{t ? `${t.where}${t.owner ? ` · ${t.owner}'s` : ""} · added ${ago(t.created)}` : "Yours, or a project's. Give it to someone and they're asked to accept it."}</DialogDescription>
        </DialogHeader>
        <Failed error={error || undefined} />
        <form onSubmit={save} className="space-y-3 text-sm">
          <Input value={form.title} onChange={(e) => set({ title: e.target.value })} placeholder="What to do" aria-label="Title" disabled={readOnly} autoFocus={!t} />
          <Textarea value={form.notes} onChange={(e) => set({ notes: e.target.value })} rows={3} placeholder="Notes" aria-label="Notes" disabled={readOnly} />
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="space-y-1">
              <span className="text-muted-foreground text-xs">Due</span>
              <Input type="date" value={form.due} onChange={(e) => set({ due: e.target.value })} aria-label="Due" disabled={readOnly} />
            </label>
            <label className="space-y-1">
              <span className="text-muted-foreground text-xs">Remind me (only you)</span>
              <Input type="datetime-local" value={form.remind} onChange={(e) => set({ remind: e.target.value })} aria-label="Remind me" />
            </label>
            <label className="space-y-1">
              <span className="text-muted-foreground text-xs">Priority</span>
              <select value={form.priority} onChange={(e) => set({ priority: e.target.value })} className={cn(field, "w-full")} aria-label="Priority" disabled={readOnly}>
                {["low", "normal", "high", "urgent"].map((p) => (
                  <option key={p}>{p}</option>
                ))}
              </select>
            </label>
            <label className="space-y-1">
              <span className="text-muted-foreground text-xs">Repeats</span>
              <select value={form.repeat} onChange={(e) => set({ repeat: e.target.value })} className={cn(field, "w-full")} aria-label="Repeats" disabled={readOnly}>
                {REPEATS.map((r) => (
                  <option key={r} value={r}>
                    {r || "never"}
                  </option>
                ))}
              </select>
            </label>
            <label className="space-y-1 sm:col-span-2">
              <span className="text-muted-foreground text-xs">Project</span>
              <select value={form.project} onChange={(e) => set({ project: e.target.value })} className={cn(field, "w-full")} aria-label="Project" disabled={readOnly}>
                <option value="">None (your own)</option>
                {projects.map((p) => (
                  <option key={p.id} value={p.name}>
                    {p.name}
                  </option>
                ))}
              </select>
            </label>
          </div>
          <div className="space-y-1">
            <span className="text-muted-foreground text-xs">Checklist</span>
            {form.items.map((i, n) => (
              <div key={`${n}-${i.text}`} className="flex items-center gap-2">
                <input type="checkbox" checked={i.done} disabled={readOnly} onChange={() => set({ items: form.items.map((x, k) => (k === n ? { ...x, done: !x.done } : x)) })} aria-label={i.text} />
                <span className={cn("flex-1", i.done && "text-muted-foreground line-through")}>{i.text}</span>
                {!readOnly && (
                  <button type="button" onClick={() => set({ items: form.items.filter((_, k) => k !== n) })} aria-label={`Remove ${i.text}`} className="text-muted-foreground hover:text-foreground">
                    <X className="size-3.5" />
                  </button>
                )}
              </div>
            ))}
            {!readOnly && (
              <div className="flex gap-2">
                <Input value={item} onChange={(e) => setItem(e.target.value)} placeholder="Add an item" aria-label="Add an item" onKeyDown={(e) => {
                  if (e.key === "Enter" && item.trim()) {
                    e.preventDefault();
                    set({ items: [...form.items, { done: false, text: item.trim() }] });
                    setItem("");
                  }
                }} />
              </div>
            )}
          </div>
          {!t && (
            <div className="space-y-1">
              <span className="text-muted-foreground text-xs">Give it to (they're asked to accept it)</span>
              <PeoplePicker people={people} value={form.assignees} onChange={(v) => set({ assignees: v })} label="Give it to" />
            </div>
          )}
          {/* With the task's people, comments and history below, the buttons are a row of the form, not the dialog's bottom bar. */}
          <DialogFooter className={cn("flex-wrap gap-2", t && inline)}>
            {t && t.mine && (
              <Button type="button" variant="ghost" className="mr-auto text-red-300" disabled={busy} onClick={() => void run("task_delete", { id: t.id }, true)}>
                <Trash2 /> Delete
              </Button>
            )}
            <Button type="button" variant="ghost" onClick={onClose}>
              Close
            </Button>
            {!readOnly && (
              <Button type="submit" disabled={busy || !form.title.trim()}>
                <Check /> {t ? "Save" : "Add"}
              </Button>
            )}
          </DialogFooter>
        </form>
        {t && (
          <div className="space-y-3 border-t pt-3 text-sm">
            {t.waiting_for_you ? (
              <div className="flex flex-wrap gap-2">
                <Button size="sm" disabled={busy} onClick={() => void run("task_answer", { id: t.id, accept: true }, true)}>
                  Accept
                </Button>
                <Button size="sm" variant="secondary" disabled={busy} onClick={() => void run("task_answer", { id: t.id, accept: false, note: comment }, true)}>
                  Decline
                </Button>
                <Input value={comment} onChange={(e) => setComment(e.target.value)} placeholder="Why (optional, when declining)" className="h-8 flex-1" />
              </div>
            ) : (
              <>
                <div className="flex flex-wrap items-center gap-2">
                  <span className="text-muted-foreground text-xs">Given to:</span>
                  {(t.assignees ?? []).map((a) => (
                    <Badge key={a.name} variant="outline" className={a.state === "pending" ? "border-amber-700/60 text-amber-300" : a.state === "declined" ? "line-through" : ""}>
                      {a.name} · {a.state === "pending" ? "asked" : a.state}
                    </Badge>
                  ))}
                  {!(t.assignees ?? []).length && <span className="text-muted-foreground text-xs">nobody</span>}
                  {people.length > 0 && (
                    <span className="ml-auto flex items-center gap-1">
                      <select value={give} onChange={(e) => setGive(e.target.value)} className={cn(field, "h-8")} aria-label="Give it to">
                        <option value="">Give it to…</option>
                        {people.map((p) => (
                          <option key={p.id} value={p.name}>
                            {p.name}
                          </option>
                        ))}
                      </select>
                      <Button size="sm" variant="secondary" disabled={busy || !give} onClick={() => void run("task_assign", { id: t.id, person: give }).then((ok) => ok && setGive(""))}>
                        <UserPlus /> Ask
                      </Button>
                    </span>
                  )}
                </div>
                <form
                  className="flex gap-2"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (comment.trim()) void run("task_comment", { id: t.id, text: comment }).then((ok) => ok && setComment(""));
                  }}
                >
                  <Input value={comment} onChange={(e) => setComment(e.target.value)} placeholder="Comment (everyone on it sees it)" aria-label="Comment" />
                  <Button size="icon" type="submit" variant="secondary" disabled={busy || !comment.trim()} aria-label="Send the comment">
                    <Send />
                  </Button>
                </form>
                {t.status === "done" && (
                  <Button size="sm" variant="secondary" disabled={busy} onClick={() => void run("task_reopen", { id: t.id }, true)}>
                    Open it again
                  </Button>
                )}
              </>
            )}
            {t.history.length > 0 && (
              <div className="space-y-1">
                <div className="text-muted-foreground text-xs">History</div>
                {t.history.map((h) => (
                  <div key={`${h.at}-${h.kind}`} className="flex justify-between gap-2 text-xs">
                    <span className="min-w-0">
                      <span className="text-muted-foreground">{h.by} · </span>
                      {h.kind === "comment" ? h.text : `${h.kind}${h.text ? `: ${h.text}` : ""}`}
                    </span>
                    <span className="shrink-0 text-muted-foreground">{ago(h.at)}</span>
                  </div>
                ))}
              </div>
            )}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

/** A project: its tasks, status updates, members; a new one when `id` is empty. */
function ProjectDialog({ id, open, onClose, people, said, openTask }: { id: string | null; open: boolean; onClose: () => void; people: People; said: (t: string) => void; openTask: (id: string) => void }) {
  const { call } = useLyra();
  type Full = ProjectBrief & { open_tasks: TaskBrief[]; updates: { at: string; by: string; health: string; text: string }[]; error?: string };
  const [p, setP] = useState<Full | null>(null);
  const [form, setForm] = useState({ name: "", description: "", due: "", status: "active", members: [] as string[] });
  const [update, setUpdate] = useState({ health: "on_track", text: "" });
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const set = (c: Partial<typeof form>) => setForm((f) => ({ ...f, ...c }));
  const load = useCallback(async (key: string) => {
    const r = await call<Full>("project", { id: key });
    if (r.error) return setError(r.error);
    setP(r);
    setForm({ name: r.name, description: r.description, due: r.due ?? "", status: r.status, members: r.members.filter((m) => m.role !== "owner").map((m) => m.name) });
    setUpdate((u) => ({ ...u, health: r.health }));
  }, [call]);
  useEffect(() => {
    if (!open) return;
    setError("");
    setUpdate({ health: "on_track", text: "" });
    if (id) void load(id);
    else {
      setP(null);
      setForm({ name: "", description: "", due: "", status: "active", members: [] });
    }
  }, [open, id, load]);
  const run = async (what: string, arg: object) => {
    setBusy(true);
    const r = await call<Answer & { project?: ProjectBrief }>(what, arg);
    setBusy(false);
    if (r.error) return setError(r.error), false;
    if (r.text) said(r.text);
    return r;
  };
  const save = async (e: FormEvent) => {
    e.preventDefault();
    const r = await run("project_save", { id: p?.id ?? "", name: form.name, description: form.description, due: form.due || "none", status: form.status, members: form.members });
    if (r && r.project) {
      said(p ? `saved ${r.project.name}` : `made project ${r.project.name}`);
      if (p) void load(r.project.id);
      else onClose();
    }
  };
  const lead = !p || p.mine;
  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-h-[90vh] overflow-y-auto sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{p ? p.name : "New project"}</DialogTitle>
          <DialogDescription>{p ? `${p.owner}'s · ${p.tasks.open} open, ${p.tasks.done} done` : "Its members see its tasks and status updates."}</DialogDescription>
        </DialogHeader>
        <Failed error={error || undefined} />
        <form onSubmit={(e) => void save(e)} className="space-y-3 text-sm">
          <Input value={form.name} onChange={(e) => set({ name: e.target.value })} placeholder="Name" aria-label="Project name" />
          <Textarea value={form.description} onChange={(e) => set({ description: e.target.value })} rows={2} placeholder="What it's for" aria-label="Description" />
          <div className="grid gap-3 sm:grid-cols-2">
            <label className="space-y-1">
              <span className="text-muted-foreground text-xs">Due</span>
              <Input type="date" value={form.due} onChange={(e) => set({ due: e.target.value })} aria-label="Project due" />
            </label>
            {p && (
              <label className="space-y-1">
                <span className="text-muted-foreground text-xs">Status</span>
                <select value={form.status} onChange={(e) => set({ status: e.target.value })} className={cn(field, "w-full")} aria-label="Status">
                  <option value="active">active</option>
                  <option value="on_hold">on hold</option>
                  <option value="done">done</option>
                </select>
              </label>
            )}
          </div>
          <div className="space-y-1">
            <span className="text-muted-foreground text-xs">Members{lead ? "" : " (its owner changes them)"}</span>
            {lead ? <PeoplePicker people={people} value={form.members} onChange={(v) => set({ members: v })} label="Members" /> : <p className="text-xs">{p?.members.map((m) => m.name).join(", ")}</p>}
          </div>
          <DialogFooter className={cn("flex-wrap gap-2", p && inline)}>
            {p?.mine && (
              <Button type="button" variant="ghost" className="mr-auto text-red-300" disabled={busy} onClick={() => void run("project_delete", { id: p.id }).then((r) => r && onClose())}>
                <Trash2 /> Delete
              </Button>
            )}
            <Button type="button" variant="ghost" onClick={onClose}>
              Close
            </Button>
            <Button type="submit" disabled={busy || !form.name.trim()}>
              <Check /> {p ? "Save" : "Make it"}
            </Button>
          </DialogFooter>
        </form>
        {p && (
          <div className="space-y-4 border-t pt-3 text-sm">
            <div className="space-y-1">
              <div className="text-muted-foreground text-xs">Open tasks</div>
              {p.open_tasks.map((x) => (
                <button key={x.id} type="button" onClick={() => openTask(x.id)} className="flex w-full justify-between gap-2 py-0.5 text-left hover:underline">
                  <span className="min-w-0 truncate">{x.title}</span>
                  <span className="shrink-0 text-muted-foreground text-xs">{x.assignees?.map((a) => a.name).join(", ")}{x.due ? ` · ${x.due}` : ""}</span>
                </button>
              ))}
              {p.open_tasks.length === 0 && <p className="text-muted-foreground text-xs">None open.</p>}
            </div>
            <div className="space-y-2">
              <div className="flex items-center justify-between">
                <span className="text-muted-foreground text-xs">Status update (members see it)</span>
                <Button
                  size="sm"
                  variant="ghost"
                  disabled={busy}
                  onClick={async () => {
                    const d = await call<{ text?: string; health?: string; error?: string }>("project_draft", { id: p.id });
                    if (d.error) setError(d.error);
                    else setUpdate({ health: d.health ?? "on_track", text: d.text ?? "" });
                  }}
                >
                  Draft it from the tasks
                </Button>
              </div>
              <div className="flex gap-2">
                <select value={update.health} onChange={(e) => setUpdate((u) => ({ ...u, health: e.target.value }))} className={field} aria-label="Health">
                  <option value="on_track">on track</option>
                  <option value="at_risk">at risk</option>
                  <option value="off_track">off track</option>
                </select>
              </div>
              <Textarea value={update.text} onChange={(e) => setUpdate((u) => ({ ...u, text: e.target.value }))} rows={4} placeholder="Progress, what's in the way, next steps" aria-label="Status update" />
              <Button size="sm" disabled={busy || !update.text.trim()} onClick={() => void run("project_update", { id: p.id, ...update }).then((r) => r && (setUpdate((u) => ({ ...u, text: "" })), void load(p.id)))}>
                <Send /> Post it
              </Button>
              {p.updates.map((u) => (
                <div key={u.at} className="rounded-md border px-3 py-2">
                  <div className="flex justify-between text-muted-foreground text-xs">
                    <span>
                      {u.by} · {HEALTH[u.health]?.text ?? u.health}
                    </span>
                    <span>{ago(u.at)}</span>
                  </div>
                  <div className="whitespace-pre-wrap">{u.text}</div>
                </div>
              ))}
            </div>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

export function TasksPage({ onBack }: { onBack: () => void }) {
  const { status, call } = useLyra();
  const view = status.tasks;
  // Quick add goes through /task add (it reads "friday 3pm" out of the words).
  const { act, busy, note } = useAction(() => {});
  const [text, setText] = useState("");
  const [addFocus] = useState(() => takeIntent("add"));
  const [tab, setTab] = useState<"tasks" | "projects" | "done">("tasks");
  const [opened, setOpened] = useState<{ id: string | null } | null>(null);
  const [project, setProject] = useState<{ id: string | null } | null>(null);
  const [people, setPeople] = useState<People>([]);
  const [done, setDone] = useState<TaskBrief[]>([]);
  const [todo, setTodo] = useState<{ on: boolean; wanted: boolean; connected: boolean; list: string; last: string | null; error: string | null } | null>(null);
  const [said, setSaid] = useState("");
  // Who tasks can go to, and what was done lately: read again as the view changes.
  useEffect(() => {
    void call<{ people?: People; done?: TaskBrief[]; todo?: typeof todo }>("tasks").then((r) => {
      setPeople(r.people ?? []);
      setDone(r.done ?? []);
      setTodo(r.todo ?? null);
    });
  }, [call, view?.at]);

  const add = async (e: FormEvent) => {
    e.preventDefault();
    if (!text.trim()) return;
    if (await act(`/task add ${text.trim()}`)) setText("");
  };
  const finish = async (x: TaskBrief) => {
    const r = await call<Answer>("task_done", { id: x.id });
    setSaid(r.error ?? r.text ?? "");
  };
  const t = today();
  const tasks = view?.tasks ?? [];
  const projects = view?.projects ?? [];
  const groups: { name: string; items: TaskBrief[] }[] = [
    { name: "Overdue", items: tasks.filter((x) => x.due && x.due < t) },
    { name: "Today", items: tasks.filter((x) => x.due === t) },
    { name: "Upcoming", items: tasks.filter((x) => x.due && x.due > t) },
    { name: "No date", items: tasks.filter((x) => !x.due) },
  ].filter((g) => g.items.length);
  const row = (x: TaskBrief) => <TaskRow key={x.id} task={x} busy={busy} onDone={() => void finish(x)} onSnooze={() => void act(`/task snooze ${x.id} 1h`)} onOpen={() => setOpened({ id: x.id })} />;

  return (
    <Page
      title="Tasks"
      description="Your tasks and reminders, ones people gave you, and your projects."
      action={
        <div className="flex items-center gap-2">
          <Button size="sm" variant="secondary" onClick={() => setOpened({ id: null })}>
            <Plus /> New task
          </Button>
          <Back onBack={onBack} />
        </div>
      }
    >
      {todo?.on && (
        <p className={cn("text-xs", todo.error ? "text-amber-300" : "text-muted-foreground")}>
          Your own tasks are kept in step with Microsoft To Do (list “{todo.list}”){todo.last ? ` · synced ${ago(todo.last)}` : " · first sync soon"}
          {todo.error ? ` · last try failed: ${todo.error}` : ""}
        </p>
      )}
      {todo && !todo.on && todo.wanted && todo.connected && <p className="text-muted-foreground text-xs">To keep your tasks in step with Microsoft To Do, connect Outlook again (Profile → Connections) and allow it.</p>}
      <TodayCard />
      <InboxCard />
      <Failed error={view?.error ?? undefined} />
      {note}
      {said && (
        <div className="flex items-start justify-between gap-2 rounded-md border border-teal-700/60 bg-teal-950/30 px-3 py-2 text-sm text-teal-100">
          <span>{said}</span>
          <button type="button" onClick={() => setSaid("")} className="text-muted-foreground hover:text-foreground" aria-label="Dismiss">
            <X className="size-4" />
          </button>
        </div>
      )}
      <form onSubmit={add} className="flex gap-2">
        <Input value={text} onChange={(e) => setText(e.target.value)} placeholder="Add a task: call the vendor friday 3pm" disabled={busy} autoFocus={addFocus} aria-label="Add a task" />
        <Button type="submit" disabled={busy || !text.trim()}>
          <Plus /> Add
        </Button>
      </form>
      <div className="flex gap-1">
        {(["tasks", "projects", "done"] as const).map((k) => (
          <Button key={k} size="sm" variant={tab === k ? "secondary" : "ghost"} onClick={() => setTab(k)}>
            {k === "tasks" ? `Tasks${tasks.length ? ` · ${tasks.length}` : ""}` : k === "projects" ? `Projects${projects.length ? ` · ${projects.length}` : ""}` : "Done"}
          </Button>
        ))}
      </div>
      {tab === "tasks" && (
        <>
          {(view?.waiting.length ?? 0) > 0 && (
            <Card className="gap-1 border-amber-700/50 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">Waiting for your answer</CardTitle>
                <CardDescription>Someone gave you these. Accept one to make it yours, or decline it.</CardDescription>
              </CardHeader>
              <CardContent className="divide-y px-4 text-sm">
                {view?.waiting.map((w) => (
                  <div key={w.id} className="flex flex-wrap items-center gap-2 py-2">
                    <button type="button" onClick={() => setOpened({ id: w.id })} className="min-w-0 flex-1 text-left">
                      <div>{w.title}</div>
                      <div className="text-muted-foreground text-xs">
                        from {w.owner ?? "someone"}
                        {w.due ? ` · due ${w.due}` : ""}
                      </div>
                    </button>
                    <Button size="sm" onClick={() => void call<Answer>("task_answer", { id: w.id, accept: true }).then((r) => setSaid(r.error ?? r.text ?? ""))}>
                      Accept
                    </Button>
                    <Button size="sm" variant="secondary" onClick={() => setOpened({ id: w.id })}>
                      Decline…
                    </Button>
                  </div>
                ))}
              </CardContent>
            </Card>
          )}
          {groups.map((g) => (
            <Card key={g.name} className="gap-0 py-3">
              <CardHeader className="px-4">
                <CardTitle className={cn("text-sm", g.name === "Overdue" && "text-amber-300")}>{g.name}</CardTitle>
                <CardAction className="text-muted-foreground text-xs">{g.items.length}</CardAction>
              </CardHeader>
              <CardContent className="divide-y px-4">{g.items.map(row)}</CardContent>
            </Card>
          ))}
          {view?.at && tasks.length === 0 && <p className="text-muted-foreground text-sm">Nothing open. Add one above, or tell lyra "remind me tomorrow at 9 to …".</p>}
          {(view?.given.length ?? 0) > 0 && (
            <Card className="gap-0 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">Given to others</CardTitle>
                <CardAction className="text-muted-foreground text-xs">{view?.given.length}</CardAction>
              </CardHeader>
              <CardContent className="divide-y px-4">{view?.given.map(row)}</CardContent>
            </Card>
          )}
          {(view?.inbox.length ?? 0) > 0 && (
            <Card className="gap-1 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">Lately on your tasks</CardTitle>
                {(view?.inbox_unread ?? 0) > 0 && <CardAction className="text-muted-foreground text-xs">{view?.inbox_unread} new</CardAction>}
              </CardHeader>
              <CardContent className="px-4 text-sm">
                {view?.inbox.slice(0, 8).map((i) => (
                  <button key={i.id} type="button" onClick={() => i.task && setOpened({ id: i.task.id })} className="flex w-full justify-between gap-2 py-1 text-left">
                    <span className="min-w-0 truncate">
                      <span className={cn("text-muted-foreground", i.unread && "text-foreground")}>
                        {i.by} {i.what === "comment" ? "commented" : i.what}
                        {i.what === "comment" && i.text ? `: ${i.text}` : ""} ·{" "}
                      </span>
                      {i.task?.title ?? ""}
                    </span>
                    <span className="shrink-0 text-muted-foreground text-xs">{ago(i.at)}</span>
                  </button>
                ))}
              </CardContent>
            </Card>
          )}
        </>
      )}
      {tab === "projects" && (
        <Card className="gap-0 py-3">
          <CardHeader className="px-4">
            <CardTitle className="flex items-center gap-2 text-sm">
              <FolderKanban className="size-4" /> Projects
            </CardTitle>
            <CardAction>
              <Button size="sm" variant="ghost" onClick={() => setProject({ id: null })}>
                <Plus /> New project
              </Button>
            </CardAction>
          </CardHeader>
          <CardContent className="divide-y px-4">
            {projects.map((p) => (
              <ProjectRow key={p.id} p={p} onOpen={() => setProject({ id: p.id })} />
            ))}
            {projects.length === 0 && <p className="py-2 text-muted-foreground text-sm">No projects yet. Make one and add people: they see its tasks and status updates.</p>}
          </CardContent>
        </Card>
      )}
      {tab === "done" && (
        <Card className="gap-0 py-3">
          <CardHeader className="px-4">
            <CardTitle className="text-sm">Done in the last two weeks</CardTitle>
          </CardHeader>
          <CardContent className="divide-y px-4">
            {done.map((x) => (
              <TaskRow key={x.id} task={x} busy={busy} onDone={() => {}} onSnooze={() => {}} onOpen={() => setOpened({ id: x.id })} />
            ))}
            {done.length === 0 && <p className="py-2 text-muted-foreground text-sm">Nothing finished lately.</p>}
          </CardContent>
        </Card>
      )}
      <TaskDialog id={opened?.id ?? null} open={!!opened} onClose={() => setOpened(null)} people={people} projects={projects} said={setSaid} />
      <ProjectDialog
        id={project?.id ?? null}
        open={!!project}
        onClose={() => setProject(null)}
        people={people}
        said={setSaid}
        openTask={(id) => {
          setProject(null);
          setOpened({ id });
        }}
      />
    </Page>
  );
}
