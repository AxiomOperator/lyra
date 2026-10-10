// Today: each person's own day at a glance — the morning briefing, the
// end-of-day recap and what lyra is watching for them.

import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { ChevronDown, CircleCheck, Eye, Moon, RefreshCw, Sun, TriangleAlert, X } from "lucide-react";
import { useState } from "react";
import { Page } from "./parts";
import { ago } from "./push";
import { useData, useLyra } from "./store";
import type { Briefing, BriefingLevel } from "./types";

const mark: Record<BriefingLevel, { icon: typeof Sun; cls: string }> = {
  attention: { icon: TriangleAlert, cls: "text-amber-400" },
  note: { icon: ChevronDown, cls: "text-muted-foreground -rotate-90" },
  ok: { icon: CircleCheck, cls: "text-emerald-500" },
};

/** "Tell me when …": what lyra is watching for, with a way to stop. */
function WatchesCard() {
  const { run } = useLyra();
  const [watches, reload] = useData<{ id: string; label: string; created: string; until: string }[] | null>("watches");
  if (!watches?.length) return null;
  return (
    <Card className="gap-2 py-4">
      <CardHeader className="px-4">
        <CardDescription className="flex items-center gap-1.5">
          <Eye className="size-4" /> Watching for
        </CardDescription>
      </CardHeader>
      <CardContent className="divide-y px-4">
        {watches.map((w) => (
          <div key={w.id} className="flex items-center gap-2 py-1.5 text-sm">
            <span className="min-w-0 flex-1">{w.label}</span>
            <span className="shrink-0 text-muted-foreground text-xs">since {ago(w.created)}</span>
            <Button
              size="icon"
              variant="ghost"
              aria-label="Stop watching"
              onClick={async () => {
                await run(`/watches cancel ${w.id}`);
                reload();
              }}
            >
              <X />
            </Button>
          </div>
        ))}
      </CardContent>
    </Card>
  );
}

/** The end-of-day recap: today, what slipped, mail waiting, tomorrow. */
function RecapCard() {
  const { run } = useLyra();
  const [recap, reload] = useData<{ at: string; parts: { title: string; lines: string[] }[] } | null>("recap");
  const [asked, setAsked] = useState(false);
  // Only once there's been one, or late in the day.
  if (!recap && new Date().getHours() < 15) return null;
  return (
    <Card className="gap-2 py-4">
      <CardHeader className="px-4">
        <CardDescription className="flex items-center gap-1.5">
          <Moon className="size-4" /> End of day{recap && ` · ${ago(recap.at)}`}
        </CardDescription>
        <CardTitle className="text-lg">{recap?.parts[0]?.title ?? "No recap yet"}</CardTitle>
        <CardAction>
          <Button
            size="sm"
            variant="outline"
            disabled={asked}
            onClick={async () => {
              setAsked(true);
              await run("/recap");
              reload();
              setAsked(false);
            }}
          >
            <RefreshCw className={cn(asked && "animate-spin")} /> Recap now
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-3 px-4">
        {!recap && <p className="text-muted-foreground text-sm">One comes at the end of your working day ([planner] day_end) and is pushed to your devices.</p>}
        {recap?.parts.map((p, i) => (
          <div key={i} className="space-y-0.5">
            {i > 0 && <div className="font-medium text-sm">{p.title}</div>}
            {p.lines.map((l, j) => (
              <div key={j} className="text-muted-foreground text-sm">
                {l}
              </div>
            ))}
          </div>
        ))}
      </CardContent>
    </Card>
  );
}

/** The daily briefing: what needs a look first; the rest folded away. */
function BriefingCard({ briefing, go }: { briefing: Briefing | null | undefined; go: (page: string) => void }) {
  const { run } = useLyra();
  const [asked, setAsked] = useState(false);
  const [open, setOpen] = useState(false);
  const items = (briefing?.sections ?? []).flatMap((s) => s.items.map((it) => ({ ...it, section: s.name })));
  const shown = items.filter((it) => it.level !== "ok");
  const fine = items.length - shown.length;
  const row = (it: (typeof items)[number], i: number) => {
    const M = mark[it.level];
    return (
      <button key={i} type="button" onClick={() => go(it.link)} className="flex w-full items-start gap-2 rounded-md px-1 py-1 text-left text-sm hover:bg-muted/50">
        <M.icon className={cn("mt-0.5 size-4 shrink-0", M.cls)} />
        <span className="min-w-0 flex-1">
          <span className="text-muted-foreground">{it.section} · </span>
          {it.text}
        </span>
      </button>
    );
  };
  return (
    <Card className={cn("gap-2 py-4", briefing?.attention ? "border-amber-700/50" : "")}>
      <CardHeader className="px-4">
        <CardDescription className="flex items-center gap-1.5">
          <Sun className="size-4" /> Briefing{briefing && ` · ${ago(briefing.at)}`}
        </CardDescription>
        <CardTitle className="text-lg">{briefing?.headline ?? "No briefing yet"}</CardTitle>
        <CardAction>
          <Button
            size="sm"
            variant="outline"
            disabled={asked}
            onClick={async () => {
              setAsked(true);
              await run("/briefing now");
              setTimeout(() => setAsked(false), 8000);
            }}
          >
            <RefreshCw className={cn(asked && "animate-spin")} /> Brief me now
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent className="space-y-1 px-4">
        {briefing?.takeaway && <p className="pb-1 text-sm">{briefing.takeaway}</p>}
        {!briefing && <p className="text-muted-foreground text-sm">One is made each morning ([briefing] schedule) and pushed to your devices.</p>}
        {shown.map(row)}
        {fine > 0 && (
          <>
            <button type="button" onClick={() => setOpen(!open)} className="text-muted-foreground flex items-center gap-1 px-1 py-1 text-sm">
              <ChevronDown className={cn("size-4 transition-transform", !open && "-rotate-90")} /> {fine} fine
            </button>
            {open && items.filter((it) => it.level === "ok").map(row)}
          </>
        )}
      </CardContent>
    </Card>
  );
}

export function TodayPage({ go }: { go: (page: string) => void }) {
  const { status } = useLyra();
  return (
    <Page title="Today" description="Your briefing, your end-of-day recap and what lyra is watching for you.">
      <BriefingCard briefing={status.briefing} go={go} />
      <RecapCard />
      <WatchesCard />
    </Page>
  );
}
