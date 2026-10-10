// Status: everything lyra depends on — models, search, APIs, its address,
// notifications, storage, backups, routines, machines — checked every
// minute by lyra serve, with uptime and latency.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardFooter, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { ChevronDown, CircleCheck, CircleX, Moon, RefreshCw, TriangleAlert } from "lucide-react";
import { useState } from "react";
import { DiagnosisNote } from "./diagnosis";
import { Page } from "./parts";
import { ago } from "./push";
import { useLyra } from "./store";
import type { CheckState, StatusRow } from "./types";

const dot: Record<CheckState, string> = {
  up: "bg-emerald-500",
  degraded: "bg-amber-400",
  down: "bg-red-500",
  off: "bg-muted-foreground/40",
};

const word: Record<CheckState, string> = { up: "up", degraded: "degraded", down: "down", off: "off" };

/** Latencies as a small line (gaps where it didn't answer). */
function Sparkline({ values }: { values: (number | null)[] }) {
  // Checks that don't time anything (or never answered) have no line.
  if (values.length < 2 || values.every((v) => v === null)) return <span className="w-24 shrink-0" />;
  const max = Math.max(1, ...values.map((v) => v ?? 0));
  const w = 96;
  const h = 22;
  const step = w / (values.length - 1);
  let d = "";
  values.forEach((v, i) => {
    if (v === null) return;
    const x = i * step;
    const y = h - 2 - (v / max) * (h - 4);
    d += `${d && values[i - 1] !== null ? "L" : "M"}${x.toFixed(1)},${y.toFixed(1)} `;
  });
  const misses = values.map((v, i) => (v === null ? i : -1)).filter((i) => i >= 0);
  return (
    <svg width={w} height={h} className="w-24 shrink-0" aria-label="latency">
      <path d={d} fill="none" stroke="currentColor" strokeWidth="1.5" className="text-teal-400" />
      {misses.map((i) => (
        <rect key={i} x={i * step - 1} y={h - 3} width="2" height="3" className="fill-red-500" />
      ))}
    </svg>
  );
}

function pct(v: number | null) {
  if (v === null) return "—";
  return v >= 99.95 ? "100%" : `${v.toFixed(1)}%`;
}

/** The models lyra calls, which an admin can mark known down. */
const MARKABLE = ["chat", "fallback", "decide", "vision"];

/** Mark a model known down (lyra stops trying it), or say it's back. */
function KnownDown({ id, known }: { id: string; known?: { text: string } }) {
  const { call } = useLyra();
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const set = async (down: boolean) => {
    setBusy(true);
    const r = await call<{ ok?: boolean; error?: string }>("known_down_set", { id, down, note });
    setBusy(false);
    setError(r?.error ?? "");
    if (r?.ok) setNote("");
  };
  return (
    <div className="space-y-1.5 rounded-md border px-3 py-2">
      {known ? (
        <>
          <p className="text-foreground/90">lyra isn't trying this model: {known.text}.</p>
          <Button size="sm" variant="secondary" onClick={() => void set(false)} disabled={busy}>
            <CircleCheck /> It's back: try it again
          </Button>
        </>
      ) : (
        <>
          <p>Already know it's down? Mark it, and lyra stops trying it {id === "chat" ? "(the fallback answers)" : id === "decide" ? "(the chat model decides)" : ""} and stops telling everyone.</p>
          <div className="flex flex-wrap gap-2">
            <input
              value={note}
              onChange={(e) => setNote(e.target.value)}
              placeholder="What's going on (optional): GPU box out until Monday"
              className="h-8 min-w-0 flex-1 rounded-md border bg-transparent px-2 text-sm"
              aria-label="Why it's down"
            />
            <Button size="sm" variant="secondary" onClick={() => void set(true)} disabled={busy}>
              <Moon /> Mark known down
            </Button>
          </div>
        </>
      )}
      {error && <p className="text-red-300">{error}</p>}
    </div>
  );
}

function CheckRow({ r, toChat, known, admin }: { r: StatusRow; toChat: () => void; known?: { text: string }; admin: boolean }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="border-b last:border-0">
      <button type="button" onClick={() => setOpen(!open)} className="flex w-full items-start gap-3 py-2.5 text-left">
        <span className={cn("mt-1.5 size-2.5 shrink-0 rounded-full", dot[r.state], r.state === "up" && "shadow-[0_0_6px] shadow-emerald-500/60")} />
        <span className="min-w-0 flex-1">
          <span className="flex flex-wrap items-baseline gap-x-2">
            <span className="font-medium text-sm">{r.name}</span>
            {known && (
              <Badge variant="outline" className="border-amber-700/60 text-amber-300">
                known down
              </Badge>
            )}
            {r.target && <span className="truncate text-muted-foreground text-xs">{r.target}</span>}
          </span>
          <span className={cn("block break-words text-xs", r.state === "down" ? "text-red-300" : r.state === "degraded" ? "text-amber-300" : "text-muted-foreground")}>{r.detail}</span>
        </span>
        <span className="hidden shrink-0 items-center gap-3 sm:flex">
          <Sparkline values={r.spark} />
          <span className="w-14 text-right text-muted-foreground text-xs tabular-nums">{r.latency_ms !== null ? `${r.latency_ms} ms` : ""}</span>
          <span className="w-14 text-right text-xs tabular-nums" title="answered in the last 24 hours">
            {pct(r.uptime_24h)}
          </span>
        </span>
        <ChevronDown className={cn("mt-1 size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} />
      </button>
      {/* Something lyra needs is down: what it found when it looked into it. Machines have theirs on their page. */}
      {r.state === "down" && r.group !== "Machines" && !known && (
        <div className="-ml-4 pb-2 pl-5.5">
          <DiagnosisNote machine="server" problem={`${r.name} is down: ${r.detail}`} diagnosisKey={`status:${r.id}`} toChat={toChat} />
        </div>
      )}
      {open && (
        <div className="space-y-2 pb-3 pl-5.5 text-muted-foreground text-xs">
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 sm:hidden">
            <Sparkline values={r.spark} />
            {r.latency_ms !== null && <span>{r.latency_ms} ms</span>}
          </div>
          {admin && MARKABLE.includes(r.id) && <KnownDown id={r.id} known={known} />}
          <div className="flex flex-wrap gap-x-4 gap-y-1">
            <span>uptime 24 h {pct(r.uptime_24h)}</span>
            <span>7 days {pct(r.uptime_7d)}</span>
            {r.since && (
              <span>
                {word[r.state]} since {ago(r.since)}
              </span>
            )}
          </div>
          {r.changes.length > 0 && (
            <ul className="space-y-0.5">
              {r.changes.map((c) => (
                <li key={c.at + c.to}>
                  <span className="text-foreground/80">{new Date(c.at).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}</span> {word[c.from]} → {word[c.to]}
                  {c.detail && ` · ${c.detail}`}
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}

/** dashboard-01's section cards: the overall state, then each area. */
function SectionCards({ rows, at, banner }: { rows: StatusRow[]; at?: string; banner: string }) {
  const area = (title: string, groups: string[]) => {
    const mine = rows.filter((r) => groups.includes(r.group) && r.state !== "off");
    const up = mine.filter((r) => r.state === "up").length;
    const bad = mine.filter((r) => r.state !== "up");
    const timed = mine.filter((r) => r.latency_ms !== null);
    const avg = timed.length ? Math.round(timed.reduce((a, r) => a + (r.latency_ms ?? 0), 0) / timed.length) : null;
    return { title, up, total: mine.length, bad, avg };
  };
  const all = area("Overall", ["Models", "Tools & APIs", "lyra", "Machines"]);
  const cards = [all, area("Models", ["Models"]), area("Tools & lyra", ["Tools & APIs", "lyra"]), area("Machines", ["Machines"])];
  return (
    <div className="grid grid-cols-1 gap-4 *:data-[slot=card]:bg-gradient-to-t *:data-[slot=card]:from-primary/5 *:data-[slot=card]:to-card *:data-[slot=card]:shadow-xs @xl/main:grid-cols-2 @5xl/main:grid-cols-4 dark:*:data-[slot=card]:bg-card">
      {cards.map((c, i) => {
        const down = c.bad.some((r) => r.state === "down");
        const ok = c.bad.length === 0;
        const Icon = ok ? CircleCheck : down ? CircleX : TriangleAlert;
        return (
          <Card key={c.title} className="@container/card">
            <CardHeader>
              <CardDescription>{c.title}</CardDescription>
              <CardTitle className="font-semibold text-2xl tabular-nums @[250px]/card:text-3xl">
                {c.total ? `${c.up}/${c.total}` : "—"}
              </CardTitle>
              <CardAction>
                <Badge variant="outline" className={cn(ok ? "text-emerald-300" : down ? "text-red-300" : "text-amber-300")}>
                  <Icon /> {ok ? "up" : down ? "down" : "degraded"}
                </Badge>
              </CardAction>
            </CardHeader>
            <CardFooter className="flex-col items-start gap-1.5 text-sm">
              <div className="line-clamp-1 flex gap-2 font-medium">
                {i === 0 ? banner : ok ? "All answering" : c.bad.map((r) => r.name).join(", ")}
              </div>
              <div className="text-muted-foreground">{i === 0 ? (at ? `checked ${ago(at)}` : "checking…") : c.avg !== null ? `${c.avg} ms on average` : `${c.total} checked`}</div>
            </CardFooter>
          </Card>
        );
      })}
    </div>
  );
}

export function StatusPage({ toMachines, toChat }: { toMachines: () => void; toChat: () => void }) {
  const { status, run, user } = useLyra();
  const [asked, setAsked] = useState(false);
  const board = status.status;
  const rows = board?.rows ?? [];
  const groups = [...new Set(rows.map((r) => r.group))];
  // Known down (marked): not news, so not counted as down here.
  const down = rows.filter((r) => r.state === "down" && !board?.known_down?.[r.id]).length;
  const degraded = rows.filter((r) => r.state === "degraded").length;
  const banner = !board
    ? { text: "Checking everything lyra depends on…", cls: "border-border" }
    : down
      ? { text: `${down} down${degraded ? ` · ${degraded} degraded` : ""}`, cls: "border-red-700/60 bg-red-950/30 text-red-200" }
      : degraded
        ? { text: `${degraded} degraded`, cls: "border-amber-700/60 bg-amber-950/20 text-amber-200" }
        : { text: "All systems normal", cls: "border-emerald-700/50 bg-emerald-950/20 text-emerald-200" };
  return (
    <Page
      title="Status"
      description="Everything lyra depends on, checked every minute."
      action={
        <Button
          size="sm"
          variant="secondary"
          disabled={asked}
          onClick={async () => {
            setAsked(true);
            await run("/status now");
            setTimeout(() => setAsked(false), 4000);
          }}
        >
          <RefreshCw className={cn(asked && "animate-spin")} /> Check now
        </Button>
      }
    >
      <SectionCards rows={rows} at={board?.at} banner={banner.text} />
      {groups.map((g) => (
        <Card key={g} className="gap-1 py-3">
          <CardHeader className="px-4">
            <CardTitle className="flex items-center justify-between text-sm">
              {g}
              {g === "Machines" && (
                <Button size="sm" variant="ghost" className="h-7 text-xs" onClick={toMachines}>
                  Manage machines
                </Button>
              )}
            </CardTitle>
          </CardHeader>
          <CardContent className="px-4">
            <div className="hidden justify-end gap-3 pr-7 text-[11px] text-muted-foreground uppercase tracking-wide sm:flex">
              <span className="w-24 text-center">last hour</span>
              <span className="w-14 text-right">now</span>
              <span className="w-14 text-right">24 h</span>
            </div>
            {rows
              .filter((r) => r.group === g)
              .map((r) => (
                <CheckRow key={r.id} r={r} toChat={toChat} known={board?.known_down?.[r.id]} admin={user?.admin ?? true} />
              ))}
          </CardContent>
        </Card>
      ))}
      {board && rows.some((r) => r.state === "off") && (
        <p className="text-muted-foreground text-xs">
          <Badge variant="outline" className="mr-1.5">
            off
          </Badge>
          means it isn't set up or is turned off in config.toml.
        </p>
      )}
    </Page>
  );
}
