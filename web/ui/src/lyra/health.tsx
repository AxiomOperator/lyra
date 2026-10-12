// How lyra is doing (admins): whether its replies are getting better. Week
// by week, how many replies went well, partly or badly, and how that's
// known (your ratings, what you did, your next message); the problems the
// detectors see now; each generation's record (and a change on trial against
// the one before it); each skill's effect; the test set. Counts only: nobody's
// words are shown.

import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { FlaskConical, TrendingUp, TriangleAlert } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Back, Failed, Page } from "./parts";
import { ago } from "./push";
import { useLyra } from "./store";

interface Arm {
  runs: number;
  judged: number;
  good: number;
  corrected: number;
}

interface Week {
  start: string;
  runs: number;
  judged: number;
  good: number;
  partial: number;
  bad: number;
  corrected: number;
  limited: number;
  rated: number;
}

interface Trial {
  share: number;
  runs: number;
  started: string;
  status: "running" | "kept" | "lost";
  note: string;
}

interface Health {
  mode: string;
  generation: number;
  weeks: Week[];
  sources: Record<string, number>;
  all: Arm;
  owner_runs: number;
  other_runs: number;
  problems: { kind: string; problem: string }[];
  generations: { number: number; reason: string; created_at: string; trial: Trial | null; record: Arm }[];
  skills: { name: string; with: Arm; without: Arm }[];
  trial: { generation: number; parent: number; trial: Arm; control: Arm; needed: number; share: number; started: string } | null;
  tests: number;
  pending: number;
}

const pct = (a: Arm) => (a.judged ? `${Math.round((a.good / a.judged) * 100)}%` : "—");
const SOURCES: Record<string, string> = { rated: "your 👍 / 👎", signal: "what you did (asked again, stopped, the limit)", judged: "your next message, read by a model", hint: "copied or read aloud", earlier: "before this was tracked" };

function Weeks({ weeks }: { weeks: Week[] }) {
  const most = Math.max(1, ...weeks.map((w) => w.runs));
  return (
    <div className="space-y-1.5">
      {weeks.map((w) => (
        <div key={w.start} className="flex items-center gap-2 text-xs">
          <span className="w-14 shrink-0 text-muted-foreground">{new Date(w.start).toLocaleDateString(undefined, { month: "short", day: "numeric" })}</span>
          <div className="flex h-3 min-w-0 flex-1 overflow-hidden rounded bg-muted" title={`${w.runs} replies: ${w.good} good, ${w.partial} partly, ${w.bad} bad, ${w.runs - w.judged} unknown`}>
            <div className="h-full bg-emerald-500/80" style={{ width: `${(w.good / most) * 100}%` }} />
            <div className="h-full bg-amber-500/80" style={{ width: `${(w.partial / most) * 100}%` }} />
            <div className="h-full bg-red-500/80" style={{ width: `${(w.bad / most) * 100}%` }} />
            <div className="h-full bg-muted-foreground/25" style={{ width: `${((w.runs - w.judged) / most) * 100}%` }} />
          </div>
          <span className="w-40 shrink-0 text-right text-muted-foreground">
            {w.runs} · {w.judged ? `${Math.round((w.good / w.judged) * 100)}% good` : "no outcomes"}
            {w.corrected ? ` · ${w.corrected} corr.` : ""}
          </span>
        </div>
      ))}
      <div className="flex flex-wrap gap-3 pt-1 text-muted-foreground text-xs">
        <span className="flex items-center gap-1"><span className="size-2 rounded-sm bg-emerald-500/80" /> good</span>
        <span className="flex items-center gap-1"><span className="size-2 rounded-sm bg-amber-500/80" /> partly</span>
        <span className="flex items-center gap-1"><span className="size-2 rounded-sm bg-red-500/80" /> bad</span>
        <span className="flex items-center gap-1"><span className="size-2 rounded-sm bg-muted-foreground/25" /> not known</span>
      </div>
    </div>
  );
}

export function HealthPage({ onBack }: { onBack: () => void }) {
  const { call, ready } = useLyra();
  const [h, setH] = useState<Health | { error: string } | null>(null);
  const load = useCallback(() => void call<Health | { error: string }>("evolution_health").then(setH), [call]);
  useEffect(() => {
    if (ready) load();
  }, [ready, load]);
  const data = h && !("error" in h) ? h : null;
  const known = data ? Object.values(data.sources).reduce((a, b) => a + b, 0) : 0;
  return (
    <Page title="How lyra is doing" description="Whether lyra's replies are getting better, and what it's learning from. Counts only: nobody's words are shown." action={<Back onBack={onBack} />}>
      {h && "error" in h && <Failed error={h.error} />}
      {data && (
        <>
          <Card className="gap-2 py-3">
            <CardHeader className="px-4">
              <CardTitle className="flex flex-wrap items-center gap-2 text-sm">
                <TrendingUp className="size-4 text-emerald-400" /> The last 8 weeks
                <Badge variant="outline">{pct(data.all)} good</Badge>
                <Badge variant="outline">generation {data.generation}</Badge>
                <Badge variant="outline">mode {data.mode}</Badge>
              </CardTitle>
              <CardDescription>
                {data.all.runs} replies, {data.all.judged} with a known outcome · {data.all.corrected} corrected · yours {data.owner_runs}, everyone else's {data.other_runs} (counted, never shown)
              </CardDescription>
            </CardHeader>
            <CardContent className="space-y-3 px-4">
              <Weeks weeks={data.weeks} />
              {known > 0 && (
                <div className="space-y-0.5 text-xs">
                  <div className="font-medium">How outcomes came in</div>
                  {Object.entries(data.sources)
                    .sort((a, b) => b[1] - a[1])
                    .map(([k, n]) => (
                      <div key={k} className="flex justify-between text-muted-foreground">
                        <span>{SOURCES[k] ?? k}</span>
                        <span>{n}</span>
                      </div>
                    ))}
                </div>
              )}
            </CardContent>
          </Card>

          {data.trial && (
            <Card className="gap-2 border-sky-800/60 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">On trial: generation {data.trial.generation}</CardTitle>
                <CardDescription>
                  On {Math.round(data.trial.share * 100)}% of replies since {ago(data.trial.started)}, against generation {data.trial.parent} on the rest. Kept only if it does at least as well, once {data.trial.needed} of its replies have an outcome.
                </CardDescription>
              </CardHeader>
              <CardContent className="grid grid-cols-2 gap-3 px-4 text-sm">
                {[
                  [`Generation ${data.trial.generation} (on trial)`, data.trial.trial],
                  [`Generation ${data.trial.parent}`, data.trial.control],
                ].map(([label, a]) => (
                  <div key={label as string} className="rounded-md border px-3 py-2">
                    <div className="text-muted-foreground text-xs">{label as string}</div>
                    <div className="font-medium text-lg">{pct(a as Arm)}</div>
                    <div className="text-muted-foreground text-xs">
                      {(a as Arm).judged} of {(a as Arm).runs} with an outcome · {(a as Arm).corrected} corrected
                    </div>
                  </div>
                ))}
              </CardContent>
            </Card>
          )}

          <Card className="gap-2 py-3">
            <CardHeader className="px-4">
              <CardTitle className="flex items-center gap-2 text-sm">
                <TriangleAlert className="size-4 text-amber-400" /> What the detectors see now
              </CardTitle>
              <CardDescription>
                {data.pending ? `${data.pending} proposed change(s) wait on the Proposals page.` : "Problems a review would propose changes for."}
              </CardDescription>
            </CardHeader>
            <CardContent className="px-4 text-sm">
              {data.problems.length ? (
                <ul className="list-inside list-disc space-y-0.5">
                  {data.problems.map((p) => (
                    <li key={p.kind + p.problem}>{p.problem}</li>
                  ))}
                </ul>
              ) : (
                <p className="text-muted-foreground">Nothing stands out in the recent replies.</p>
              )}
            </CardContent>
          </Card>

          <Card className="gap-2 py-3">
            <CardHeader className="px-4">
              <CardTitle className="text-sm">Generations</CardTitle>
              <CardDescription>Every change lyra made to itself, and how replies went under it.</CardDescription>
            </CardHeader>
            <CardContent className="space-y-1.5 px-4 text-sm">
              {data.generations.map((g) => (
                <div key={g.number} className="flex flex-wrap items-baseline gap-x-2 border-b pb-1.5 last:border-0">
                  <span className="font-medium">g{g.number}</span>
                  <span className="min-w-0 flex-1 break-words text-muted-foreground text-xs">{g.reason}</span>
                  {g.trial && <Badge variant="secondary" className={cn(g.trial.status === "lost" && "bg-red-500/20 text-red-300", g.trial.status === "kept" && "bg-emerald-500/20 text-emerald-300")}>trial {g.trial.status}</Badge>}
                  <span className="text-xs">
                    {pct(g.record)} good · {g.record.judged}/{g.record.runs}
                  </span>
                  <span className="text-muted-foreground text-xs">{ago(g.created_at)}</span>
                </div>
              ))}
            </CardContent>
          </Card>

          {data.skills.length > 0 && (
            <Card className="gap-2 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">Skills' effect</CardTitle>
                <CardDescription>How replies that used a skill went, against those that didn't (only the skills a reply followed get credit).</CardDescription>
              </CardHeader>
              <CardContent className="space-y-1 px-4 text-sm">
                {data.skills.map((s) => (
                  <div key={s.name} className="flex flex-wrap items-baseline justify-between gap-x-3">
                    <span className="font-mono text-xs">{s.name}</span>
                    <span className="text-xs">
                      with it {pct(s.with)} ({s.with.judged}/{s.with.runs}) · without {pct(s.without)}
                    </span>
                  </div>
                ))}
              </CardContent>
            </Card>
          )}

          <Card className="gap-2 py-3">
            <CardHeader className="px-4">
              <CardTitle className="flex items-center gap-2 text-sm">
                <FlaskConical className="size-4 text-sky-400" /> Tests: {data.tests}
              </CardTitle>
              <CardDescription>
                Changes are measured on these before they're proposed: replies you rated 👍, and any you add with "Add to tests" under a reply. Each change is compared pair by pair with the current lyra, judged blind, and passes only when it's better beyond the noise.
              </CardDescription>
            </CardHeader>
          </Card>
        </>
      )}
    </Page>
  );
}
