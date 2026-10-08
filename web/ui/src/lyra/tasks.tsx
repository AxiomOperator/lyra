// Tasks: the user's PMI tasks (personal and assigned), what waits on them,
// and their projects' health. Live: lyra serve follows PMI's events and
// sends the view with every change. Changes go through lyra's commands.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { AlarmClock, Check, Circle, Plus, Radio } from "lucide-react";
import { useState, type FormEvent } from "react";
import { InboxCard, TodayCard } from "./calendar";
import { Back, Failed, Page, useAction } from "./parts";
import { ago } from "./push";
import { useLyra } from "./store";
import type { PmiProject, PmiTask } from "./types";

const today = () => {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};

function dueText(due: string | null) {
  if (!due) return null;
  const t = today();
  if (due < t) return { text: `overdue · ${due}`, cls: "text-amber-400" };
  if (due === t) return { text: "today", cls: "text-teal-300" };
  return { text: new Date(`${due}T12:00`).toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" }), cls: "text-muted-foreground" };
}

const health: Record<string, { text: string; cls: string }> = {
  on_track: { text: "on track", cls: "border-emerald-700/60 text-emerald-300" },
  at_risk: { text: "at risk", cls: "border-amber-700/60 text-amber-300" },
  off_track: { text: "off track", cls: "border-red-700/60 text-red-300" },
  no_tasks: { text: "no tasks", cls: "text-muted-foreground" },
};

function TaskRow({ task, onDone, onSnooze, busy }: { task: PmiTask; onDone: () => void; onSnooze: () => void; busy: boolean }) {
  const due = dueText(task.due);
  return (
    <div className="flex items-start gap-3 py-2">
      <button type="button" disabled={busy} onClick={onDone} aria-label="Done" className="mt-0.5 text-muted-foreground hover:text-teal-300 disabled:opacity-50">
        <Circle className="size-5" />
      </button>
      <div className="min-w-0 flex-1">
        <div className="text-sm">{task.title}</div>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-muted-foreground text-xs">
          {due && <span className={due.cls}>{due.text}</span>}
          <span>{task.where}</span>
          {task.status === "in_progress" && <span>in progress</span>}
          {(task.priority === "high" || task.priority === "urgent") && <span className="text-amber-300">{task.priority}</span>}
          {task.checklist && <span>☑ {task.checklist}</span>}
          {task.repeat && <span>↻ {task.repeat}</span>}
          {task.blocked && <span className="text-red-300">blocked</span>}
        </div>
      </div>
      <Button size="icon" variant="ghost" disabled={busy} onClick={onSnooze} aria-label="Remind me in an hour" title="Remind me in an hour">
        <AlarmClock />
      </Button>
    </div>
  );
}

function ProjectRow({ p }: { p: PmiProject }) {
  const h = health[p.health] ?? health.no_tasks;
  const pct = p.tasks.total ? Math.round((p.tasks.done / p.tasks.total) * 100) : 0;
  return (
    <div className="flex items-center gap-3 py-2">
      <div className="min-w-0 flex-1">
        <div className="text-sm">{p.name}</div>
        <div className="text-muted-foreground text-xs">
          {p.tasks.open} open{p.tasks.overdue ? ` · ${p.tasks.overdue} overdue` : ""}
          {p.risks.open ? ` · ${p.risks.open} risk${p.risks.open === 1 ? "" : "s"}` : ""}
          {p.last_update ? ` · updated ${ago(p.last_update)}` : " · no status update yet"}
        </div>
        <div className="mt-1 h-1 overflow-hidden rounded bg-muted">
          <div className="h-full bg-teal-500/70" style={{ width: `${pct}%` }} />
        </div>
      </div>
      <Badge variant="outline" className={h.cls}>
        {h.text}
      </Badge>
    </div>
  );
}

export function TasksPage({ onBack }: { onBack: () => void }) {
  const { status } = useLyra();
  const pmi = status.pmi;
  // The view updates by itself; commands just show their answer.
  const { act, busy, note } = useAction(() => {});
  const [text, setText] = useState("");
  const [finishing, setFinishing] = useState<PmiTask | null>(null);
  const [comment, setComment] = useState("");
  const [tab, setTab] = useState<"tasks" | "projects">("tasks");

  const add = async (e: FormEvent) => {
    e.preventDefault();
    if (!text.trim()) return;
    if (await act(`/task add ${text.trim()}`)) setText("");
  };
  const t = today();
  const tasks = pmi?.tasks ?? [];
  const groups: { name: string; items: PmiTask[] }[] = [
    { name: "Overdue", items: tasks.filter((x) => x.due && x.due < t) },
    { name: "Today", items: tasks.filter((x) => x.due === t) },
    { name: "Upcoming", items: tasks.filter((x) => x.due && x.due > t) },
    { name: "No date", items: tasks.filter((x) => !x.due) },
  ].filter((g) => g.items.length);
  const waiting = [
    ...(pmi?.waiting.task_transfers ?? []).map((w) => `Task handed to you: ${w.task}`),
    ...(pmi?.waiting.project_transfers ?? []).map((w) => `Project handed to you: ${w.project}`),
    ...(pmi?.waiting.approvals ?? []).map((a) => `Approval asked: ${a.task?.title ?? "a task"}${a.by ? ` (${a.by})` : ""}`),
  ];

  return (
    <Page
      title="Tasks"
      description={pmi?.at ? `${pmi.user} · ${pmi.org} in PMI` : "Your tasks and projects in PMI."}
      action={
        <div className="flex items-center gap-2">
          {pmi?.at && (
            <span className={cn("flex items-center gap-1 text-xs", pmi.live ? "text-emerald-400" : "text-muted-foreground")} title={pmi.live ? "Following PMI's live updates" : "Reconnecting to PMI"}>
              <Radio className="size-3.5" /> {pmi.live ? "live" : ago(pmi.at)}
            </span>
          )}
          <Back onBack={onBack} />
        </div>
      }
    >
      {!pmi?.at && !pmi?.error && (
        <p className="text-muted-foreground text-sm">
          PMI isn't connected yet. Make an access token in PMI (Your account → Security), then on the server run <code>lyra pmi token</code> or send <code>/pmi token &lt;token&gt;</code> here in the chat.
        </p>
      )}
      <TodayCard />
      <InboxCard />
      <Failed error={pmi?.error ?? undefined} />
      {note}
      <form onSubmit={add} className="flex gap-2">
        <Input value={text} onChange={(e) => setText(e.target.value)} placeholder="Add a task: call the vendor friday 3pm" disabled={busy} />
        <Button type="submit" disabled={busy || !text.trim()}>
          <Plus /> Add
        </Button>
      </form>
      <div className="flex gap-1">
        {(["tasks", "projects"] as const).map((k) => (
          <Button key={k} size="sm" variant={tab === k ? "secondary" : "ghost"} onClick={() => setTab(k)}>
            {k === "tasks" ? `Tasks${tasks.length ? ` · ${tasks.length}` : ""}` : `Projects${pmi?.projects.length ? ` · ${pmi.projects.length}` : ""}`}
          </Button>
        ))}
      </div>
      {tab === "tasks" && (
        <>
          {waiting.length > 0 && (
            <Card className="gap-1 border-amber-700/50 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">Waiting on you</CardTitle>
                <CardDescription>Accept or decide these in PMI.</CardDescription>
              </CardHeader>
              <CardContent className="px-4 text-sm">
                {waiting.map((w) => (
                  <div key={w} className="py-1">
                    {w}
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
              <CardContent className="divide-y px-4">
                {g.items.map((x) => (
                  <TaskRow
                    key={x.id}
                    task={x}
                    busy={busy}
                    onDone={() => {
                      setComment("");
                      setFinishing(x);
                    }}
                    onSnooze={() => void act(`/task snooze ${x.id} 1h`)}
                  />
                ))}
              </CardContent>
            </Card>
          ))}
          {pmi?.at && tasks.length === 0 && <p className="text-muted-foreground text-sm">Nothing open. Add one above, or tell lyra "remind me tomorrow at 9 to …".</p>}
          {(pmi?.inbox_unread ?? 0) > 0 && (
            <Card className="gap-1 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">PMI inbox</CardTitle>
                <CardAction className="text-muted-foreground text-xs">{pmi?.inbox_unread} unread</CardAction>
              </CardHeader>
              <CardContent className="px-4 text-sm">
                {(pmi?.inbox ?? []).slice(0, 8).map((i) => (
                  <div key={i.id} className="flex justify-between gap-2 py-1">
                    <span className="min-w-0 truncate">
                      <span className="text-muted-foreground">{i.by ? `${i.by} · ` : ""}{i.what ?? i.reason} · </span>
                      {i.task?.title ?? i.points ?? ""}
                    </span>
                    <span className="shrink-0 text-muted-foreground text-xs">{ago(i.at)}</span>
                  </div>
                ))}
              </CardContent>
            </Card>
          )}
        </>
      )}
      {tab === "projects" && (
        <Card className="gap-0 py-3">
          <CardContent className="divide-y px-4">
            {(pmi?.projects ?? []).map((p) => (
              <ProjectRow key={p.id} p={p} />
            ))}
            {(pmi?.projects.length ?? 0) === 0 && <p className="py-2 text-muted-foreground text-sm">No open projects you can see.</p>}
          </CardContent>
        </Card>
      )}
      <Dialog open={!!finishing} onOpenChange={(o) => !o && setFinishing(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Done: {finishing?.title}</DialogTitle>
            <DialogDescription>PMI keeps a closing comment. Leave it empty for "Done (via lyra)".</DialogDescription>
          </DialogHeader>
          <Input value={comment} onChange={(e) => setComment(e.target.value)} placeholder="What happened (optional)" autoFocus />
          <DialogFooter>
            <Button variant="ghost" onClick={() => setFinishing(null)}>
              Cancel
            </Button>
            <Button
              disabled={busy}
              onClick={async () => {
                const x = finishing;
                setFinishing(null);
                if (x) await act(`/task done ${x.id}${comment.trim() ? ` ${comment.trim()}` : ""}`);
              }}
            >
              <Check /> Done
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </Page>
  );
}
