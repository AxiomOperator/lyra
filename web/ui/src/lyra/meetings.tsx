// Meetings: one page per meeting. Before, the prep (who's coming, the last
// mail with them, open tasks); during, your notes (saved as you type); after,
// the follow-up: a summary, decisions, your action items (each one a PMI
// task with a tap) and a mail to the attendees (a draft in your Outlook:
// nothing is sent from here).

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { ArrowLeft, Check, ClipboardList, ExternalLink, Mail, Plus, Sparkles, Users } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { MessageResponse } from "@/components/ai-elements/message";
import { Back, Failed, Page } from "./parts";
import { useLyra } from "./store";

type State = "before" | "during" | "after";
interface Row {
  id: string;
  subject: string;
  start: string;
  when: string;
  people: number;
  teams: boolean;
  state: State;
  notes: boolean;
  followup: boolean;
}
interface FollowUp {
  summary?: string;
  decisions?: string[];
  mine?: { task: string; due?: string }[];
  others?: { who: string; task: string }[];
  email?: { subject?: string; body?: string };
  from?: string;
}
interface Workspace {
  id: string;
  subject: string;
  when: string;
  until: string;
  state: State;
  location?: string | null;
  join?: string | null;
  organizer?: string | null;
  agenda?: string | null;
  attendees: { name: string; email: string; response?: string }[];
  prep: string[];
  notes: string;
  followup: FollowUp | null;
  transcripts: boolean;
  error?: string;
}

const LABEL: Record<State, string> = { before: "coming up", during: "now", after: "done" };

function day(iso: string) {
  const d = new Date(iso);
  const today = new Date();
  const diff = Math.round((new Date(d.toDateString()).getTime() - new Date(today.toDateString()).getTime()) / 86400000);
  return diff === 0 ? "Today" : diff === -1 ? "Yesterday" : diff === 1 ? "Tomorrow" : d.toLocaleDateString(undefined, { weekday: "long", month: "short", day: "numeric" });
}

/** The notes box: saved a second after you stop typing. */
function Notes({ id, initial }: { id: string; initial: string }) {
  const { call } = useLyra();
  const [text, setText] = useState(initial);
  const [saved, setSaved] = useState<"" | "saving" | "saved" | string>("");
  const first = useRef(true);
  useEffect(() => {
    if (first.current) {
      first.current = false;
      return;
    }
    setSaved("saving");
    const t = window.setTimeout(() => {
      void call<{ ok?: boolean; error?: string }>("meeting_notes", { id, notes: text }).then((r) => setSaved(r?.ok ? "saved" : r?.error ?? "not saved"));
    }, 1000);
    return () => window.clearTimeout(t);
  }, [text, id, call]);
  return (
    <div className="space-y-1">
      <Textarea value={text} onChange={(e) => setText(e.target.value)} rows={8} placeholder="What's said, decided, and who does what…" aria-label="Meeting notes" />
      <p className="text-muted-foreground text-xs">{saved === "saving" ? "Saving…" : saved === "saved" ? "Saved" : saved}</p>
    </div>
  );
}

function FollowUpView({ w, followup, setFollowup }: { w: Workspace; followup: FollowUp | null; setFollowup: (f: FollowUp) => void }) {
  const { call, run } = useLyra();
  const [busy, setBusy] = useState("");
  const [note, setNote] = useState("");
  const [added, setAdded] = useState<Set<number>>(new Set());
  const write = async () => {
    setBusy("Writing the follow-up…");
    setNote("");
    const r = await call<{ followup?: FollowUp; error?: string }>("meeting_followup", { id: w.id });
    setBusy("");
    if (r?.followup) {
      setFollowup(r.followup);
      setAdded(new Set());
    } else setNote(r?.error ?? "lyra didn't answer");
  };
  const addTask = async (i: number) => {
    const t = followup?.mine?.[i];
    if (!t) return;
    const r = await run(`/task add ${t.task}${t.due ? ` ${t.due}` : ""}`);
    if (r.ok) setAdded((s) => new Set(s).add(i));
    else setNote(r.text);
  };
  const draft = async () => {
    setBusy("Making the draft…");
    const r = await call<{ draft?: unknown; error?: string }>("meeting_draft", { id: w.id });
    setBusy("");
    setNote(r?.error ?? "It's in your Outlook Drafts: look it over and send it from there (or ask lyra to send it).");
  };
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <Button onClick={() => void write()} disabled={!!busy}>
          <Sparkles /> {followup ? "Write it again" : "Write the follow-up"}
        </Button>
        <span className="text-muted-foreground text-xs">{busy || (w.transcripts ? "From the Teams transcript, or your notes." : "From your notes (and the agenda).")}</span>
      </div>
      {note && <p className="rounded-md border px-3 py-2 text-sm">{note}</p>}
      {followup && (
        <div className="space-y-3 text-sm">
          {followup.summary && <p>{followup.summary}</p>}
          {!!followup.decisions?.length && (
            <div>
              <h4 className="mb-1 font-medium text-muted-foreground text-xs uppercase">Decisions</h4>
              <ul className="list-disc space-y-0.5 pl-5">
                {followup.decisions.map((d, i) => (
                  <li key={i}>{d}</li>
                ))}
              </ul>
            </div>
          )}
          {!!followup.mine?.length && (
            <div>
              <h4 className="mb-1 font-medium text-muted-foreground text-xs uppercase">Yours to do</h4>
              <div className="space-y-1">
                {followup.mine.map((t, i) => (
                  <div key={i} className="flex items-center justify-between gap-2 rounded-md border px-3 py-1.5">
                    <span>
                      {t.task}
                      {t.due && <span className="text-muted-foreground"> · {t.due}</span>}
                    </span>
                    <Button size="sm" variant={added.has(i) ? "ghost" : "secondary"} disabled={added.has(i)} onClick={() => void addTask(i)}>
                      {added.has(i) ? <Check /> : <Plus />} {added.has(i) ? "Added" : "Add as task"}
                    </Button>
                  </div>
                ))}
              </div>
            </div>
          )}
          {!!followup.others?.length && (
            <div>
              <h4 className="mb-1 font-medium text-muted-foreground text-xs uppercase">Others</h4>
              <ul className="space-y-0.5">
                {followup.others.map((o, i) => (
                  <li key={i}>
                    <span className="font-medium">{o.who}:</span> {o.task}
                  </li>
                ))}
              </ul>
            </div>
          )}
          {followup.email?.body && (
            <div className="rounded-md border p-3">
              <div className="mb-2 flex items-center justify-between gap-2">
                <span className="font-medium">{followup.email.subject || `Follow-up: ${w.subject}`}</span>
                <Button size="sm" variant="secondary" onClick={() => void draft()} disabled={!!busy}>
                  <Mail /> Draft in Outlook
                </Button>
              </div>
              <MessageResponse>{followup.email.body}</MessageResponse>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

function MeetingView({ id, back }: { id: string; back: () => void }) {
  const { call } = useLyra();
  const [w, setW] = useState<Workspace | null>(null);
  const [followup, setFollowup] = useState<FollowUp | null>(null);
  useEffect(() => {
    setW(null);
    void call<Workspace>("meeting", { id }).then((x) => {
      setW(x);
      setFollowup(x?.followup ?? null);
    });
  }, [id, call]);
  if (!w) return <p className="text-muted-foreground text-sm">Loading the meeting…</p>;
  if (w.error) return <Failed error={w.error} />;
  return (
    <div className="space-y-4">
      <div className="flex items-start gap-2">
        <Button size="icon" variant="ghost" onClick={back} aria-label="All meetings" className="lg:hidden">
          <ArrowLeft />
        </Button>
        <div className="min-w-0">
          <h2 className="font-semibold text-lg leading-tight">{w.subject}</h2>
          <p className="text-muted-foreground text-sm">
            {w.when} – {w.until.split(" ").pop()}
            {w.location ? ` · ${w.location}` : ""}
            {w.organizer ? ` · ${w.organizer}` : ""}
          </p>
        </div>
        {w.join && (
          <Button size="sm" variant="secondary" className="ml-auto shrink-0" onClick={() => window.open(w.join ?? "", "_blank")}>
            <ExternalLink /> Join
          </Button>
        )}
      </div>
      <Card className={cn("gap-2 py-4", w.state === "before" && "border-teal-700/50")}>
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <ClipboardList className="size-4" /> Before
          </CardTitle>
          <CardDescription>Who's coming, your last mail with them, open tasks.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-2 px-4 text-sm">
          {w.prep.length ? (
            <ul className="space-y-0.5">
              {w.prep.map((l, i) => (
                <li key={i}>{l}</li>
              ))}
            </ul>
          ) : (
            <p className="text-muted-foreground">Nothing to prepare that lyra can see.</p>
          )}
          {!!w.attendees.length && (
            <p className="flex flex-wrap items-center gap-1 text-muted-foreground text-xs">
              <Users className="size-3" />
              {w.attendees.map((a) => a.name || a.email).join(", ")}
            </p>
          )}
          {w.agenda && <p className="whitespace-pre-wrap text-muted-foreground text-xs">{w.agenda}</p>}
        </CardContent>
      </Card>
      <Card className={cn("gap-2 py-4", w.state === "during" && "border-teal-700/50")}>
        <CardHeader className="px-4">
          <CardTitle className="text-base">During</CardTitle>
          <CardDescription>Your notes: kept for this meeting, and used for the follow-up.</CardDescription>
        </CardHeader>
        <CardContent className="px-4">
          <Notes id={w.id} initial={w.notes} />
        </CardContent>
      </Card>
      <Card className={cn("gap-2 py-4", w.state === "after" && "border-teal-700/50")}>
        <CardHeader className="px-4">
          <CardTitle className="text-base">After</CardTitle>
          <CardDescription>A summary, decisions, your action items and a mail to the attendees.</CardDescription>
          {followup?.from && (
            <CardAction>
              <Badge variant="outline">from the {followup.from}</Badge>
            </CardAction>
          )}
        </CardHeader>
        <CardContent className="px-4">
          <FollowUpView w={w} followup={followup} setFollowup={setFollowup} />
        </CardContent>
      </Card>
    </div>
  );
}

export function MeetingsPage({ onBack }: { onBack: () => void }) {
  const { call, ready } = useLyra();
  const [data, setData] = useState<{ connected?: boolean; meetings?: Row[]; error?: string } | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const load = useCallback(() => void call<typeof data>("meetings").then(setData), [call]);
  useEffect(() => {
    if (ready) load();
  }, [ready, load]);
  const rows = data?.meetings ?? [];
  // The one happening now, else the next one, opens first on a wide screen.
  useEffect(() => {
    if (open || !rows.length || window.innerWidth < 1024) return;
    setOpen((rows.find((r) => r.state === "during") ?? rows.find((r) => r.state === "before") ?? rows[rows.length - 1]).id);
  }, [rows, open]);
  const days = rows.reduce<{ day: string; rows: Row[] }[]>((out, r) => {
    const d = day(r.start);
    const last = out[out.length - 1];
    if (last?.day === d) last.rows.push(r);
    else out.push({ day: d, rows: [r] });
    return out;
  }, []);
  return (
    <Page title="Meetings" description="Prepare, take notes, and follow up: one page per meeting." action={<Back onBack={onBack} />}>
      {data?.error && <Failed error={data.error} />}
      {data?.connected === false && <p className="text-muted-foreground text-sm">Connect your Outlook first: Profile → Connections → Outlook → Connect.</p>}
      <div className="grid gap-4 lg:grid-cols-[18rem_1fr]">
        <div className={cn("space-y-3", open && "max-lg:hidden")}>
          {data?.connected && !rows.length && <p className="text-muted-foreground text-sm">No meetings from yesterday to the next few days.</p>}
          {days.map((d) => (
            <div key={d.day}>
              <h3 className="mb-1 font-medium text-muted-foreground text-xs uppercase">{d.day}</h3>
              <div className="space-y-1">
                {d.rows.map((r) => (
                  <button
                    key={r.id}
                    type="button"
                    onClick={() => setOpen(r.id)}
                    className={cn("w-full rounded-md border px-3 py-2 text-left hover:bg-accent/60", open === r.id && "border-teal-700/60 bg-teal-950/30")}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className="truncate font-medium text-sm">{r.subject}</span>
                      <Badge variant="outline" className={cn("shrink-0", r.state === "during" && "border-teal-600 text-teal-300")}>
                        {LABEL[r.state]}
                      </Badge>
                    </div>
                    <div className="text-muted-foreground text-xs">
                      {r.when.split(" ").pop()}
                      {r.people > 0 && ` · ${r.people} invited`}
                      {r.notes && " · notes"}
                      {r.followup && " · followed up"}
                    </div>
                  </button>
                ))}
              </div>
            </div>
          ))}
        </div>
        <div className={cn(!open && "max-lg:hidden")}>{open ? <MeetingView key={open} id={open} back={() => setOpen(null)} /> : <p className="text-muted-foreground text-sm max-lg:hidden">Pick a meeting.</p>}</div>
      </div>
    </Page>
  );
}
