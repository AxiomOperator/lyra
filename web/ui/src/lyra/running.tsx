// Running now (admins): every routine, problem write-up, plan and reply at
// work, anyone's, with how long it's run, what it's doing, what it has cost
// so far, and Stop; and the browser sandbox's containers on its machine
// (whose conversation each is for, its page, CPU and memory), each stoppable.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { Container, Square } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Back, Page, useConfirm } from "./parts";
import { ago } from "./push";
import { useLyra } from "./store";

interface Job {
  session: string;
  who: string;
  kind: "routine" | "diagnosis" | "plan" | "reply";
  what: string;
  since: string | null;
  doing: string;
  calls: number;
  tokens_in: number;
  tokens_out: number;
  cost: number;
  stoppable: boolean;
  /** Its browser in the sandbox, when it has one open. */
  browser?: { url: string; title: string } | null;
}

const KIND: Record<Job["kind"], string> = { routine: "routine", diagnosis: "problem write-up", plan: "plan", reply: "reply" };

function tokens(n: number) {
  return n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${Math.round(n / 1000)}k` : `${n}`;
}

interface SandboxContainer {
  name: string;
  running_for: string;
  status: string;
  session: string | null;
  url: string | null;
  title: string | null;
  idle_seconds: number | null;
  cpu: string | null;
  memory: string | null;
  conversation?: string;
  who?: string;
}

interface SandboxLive {
  enabled: boolean;
  machine?: string;
  ready?: boolean;
  why?: string;
  image?: string | null;
  lan_blocked?: boolean | null;
  /** How many it runs at once (from the machine's cores and memory, or set). */
  limit?: number;
  containers?: SandboxContainer[];
  /** Conversations waiting in line for a browser, in order. */
  queued?: { session: string; position: number; since: string; conversation?: string; who?: string }[];
  error?: string;
}

/** The browser sandbox's containers on its machine, live (asked every 10 s). */
function SandboxContainers() {
  const { call, ready } = useLyra();
  const [data, setData] = useState<SandboxLive | null>(null);
  const [said, setSaid] = useState("");
  const [confirm, dialog] = useConfirm();
  const load = useCallback(() => void call<SandboxLive>("sandbox_containers").then((d) => d && setData(d)), [call]);
  useEffect(() => {
    if (!ready) return;
    load();
    const t = window.setInterval(load, 10000);
    return () => window.clearInterval(t);
  }, [ready, load]);
  if (!data?.enabled) return null;
  const list = data.containers ?? [];
  const stop = async (c: SandboxContainer) => {
    const r = await call<{ ok?: boolean; error?: string }>("sandbox_kill", { container: c.name });
    setSaid(r?.ok ? `Stopped ${c.name}.` : (r?.error ?? "lyra didn't answer"));
    load();
  };
  return (
    <Card className="gap-1 py-3">
      <CardHeader className="px-4">
        <CardTitle className="flex items-center gap-2 text-sm">
          <Container className="size-4 text-sky-400" /> Browser sandbox · {data.machine}
        </CardTitle>
        <CardDescription>
          {data.error
            ? `Couldn't ask it: ${data.error}`
            : [
                data.ready ? "ready" : `not ready (${data.why})`,
                data.image,
                data.lan_blocked ? "LAN blocked" : null,
                data.limit ? `${list.length} of ${data.limit} in use` : `${list.length} container${list.length === 1 ? "" : "s"} running`,
                data.queued?.length ? `${data.queued.length} waiting` : null,
              ]
                .filter(Boolean)
                .join(" · ")}
        </CardDescription>
        <CardAction>
          <Button size="sm" variant="ghost" className="h-7 text-xs" onClick={load}>
            Refresh
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent className="divide-y px-4">
        {said && <p className="py-1 text-muted-foreground text-xs">{said}</p>}
        {!data.error && !list.length && !data.queued?.length && <p className="py-2 text-muted-foreground text-sm">No browser containers running.</p>}
        {(data.queued ?? []).map((q) => (
          <div key={q.session} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2">
            <span className="size-2 shrink-0 rounded-full border border-sky-400" title="waiting for a browser" />
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm">{q.conversation || `conversation ${q.session}`}</div>
              <div className="truncate text-muted-foreground text-xs">
                {[q.who && `for ${q.who}`, `waiting for a browser: ${q.position === 1 ? "next in line" : `number ${q.position} in line`}`, `since ${ago(q.since)}`].filter(Boolean).join(" · ")}
              </div>
            </div>
          </div>
        ))}
        {list.map((c) => (
          <div key={c.name} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2">
            <span className={cn("size-2 shrink-0 rounded-full", c.session ? "bg-emerald-500" : "bg-amber-400")} title={c.session ? "in use by a conversation" : "not tied to a conversation (left over): Stop removes it"} />
            <div className="min-w-0 flex-1">
              <div className="truncate text-sm">{c.conversation ? `${c.conversation}` : c.session ? `conversation ${c.session}` : "left over (no conversation)"}</div>
              <div className="truncate text-muted-foreground text-xs" title={c.url ?? ""}>
                {[c.who && `for ${c.who}`, c.title || c.url, `up ${c.running_for.replace(/ ago$/, "")}`, c.idle_seconds !== null && c.idle_seconds > 30 ? `idle ${Math.round(c.idle_seconds / 60)} min` : null, c.cpu && `CPU ${c.cpu}`, c.memory && `memory ${c.memory.split(" / ")[0]}`, c.name].filter(Boolean).join(" · ")}
              </div>
            </div>
            <Button
              size="sm"
              variant="ghost"
              className="text-red-400 hover:text-red-300"
              onClick={() => confirm({ title: `Stop ${c.name}?`, text: c.session ? "Its conversation's browser closes; what the reply did so far stays." : "It's not tied to any conversation.", action: "Stop", run: () => void stop(c) })}
            >
              <Square /> Stop
            </Button>
          </div>
        ))}
      </CardContent>
      {dialog}
    </Card>
  );
}

export function RunningPage({ onBack }: { onBack: () => void }) {
  const { call, ready } = useLyra();
  const [jobs, setJobs] = useState<Job[] | null>(null);
  const [said, setSaid] = useState("");
  const [confirm, dialog] = useConfirm();
  const load = useCallback(() => void call<Job[]>("running").then((d) => setJobs(Array.isArray(d) ? d : [])), [call]);
  useEffect(() => {
    if (!ready) return;
    load();
    const t = window.setInterval(load, 3000);
    return () => window.clearInterval(t);
  }, [ready, load]);
  const stop = async (j: Job) => {
    const r = await call<{ ok?: boolean; error?: string }>("running_stop", { session: j.session });
    setSaid(r?.ok ? `Stopping ${j.what || KIND[j.kind]}…` : (r?.error ?? "lyra didn't answer"));
    load();
  };
  return (
    <Page title="Running now" description="Everything lyra is working on, for everyone: how long, what it's doing, what it has cost so far; and the browser sandbox's containers." action={<Back onBack={onBack} />}>
      {said && <p className="text-muted-foreground text-sm">{said}</p>}
      <SandboxContainers />
      {jobs && !jobs.length && <p className="py-6 text-center text-muted-foreground text-sm">Nothing running right now.</p>}
      {(jobs ?? []).map((j) => (
        <Card key={j.session} className="gap-1 py-3">
          <CardContent className="flex flex-wrap items-center gap-x-3 gap-y-1 px-4">
            <Badge variant="outline">{KIND[j.kind]}</Badge>
            <span className="min-w-0 flex-1 truncate font-medium text-sm">{j.what || "(untitled)"}</span>
            <span className="text-muted-foreground text-xs">for {j.who}</span>
            {j.stoppable && (
              <Button
                size="sm"
                variant="ghost"
                className="text-red-300 hover:text-red-200"
                onClick={() => confirm({ title: `Stop ${j.what || KIND[j.kind]}?`, text: `It's ${j.who}'s: what it did so far stays.`, action: "Stop", run: () => void stop(j) })}
              >
                <Square /> Stop
              </Button>
            )}
            <div className="w-full text-muted-foreground text-xs">
              {j.since ? `started ${ago(j.since)} · ` : ""}now: {j.doing}
              {j.calls > 0 && ` · ${j.calls} model call${j.calls === 1 ? "" : "s"} · ${tokens(j.tokens_in)} in / ${tokens(j.tokens_out)} out`}
              {j.cost > 0 && ` · ${j.cost < 0.01 ? "<0.01" : j.cost.toFixed(2)} so far`}
            </div>
            {j.browser && (
              <div className="w-full text-sky-300 text-xs" title={j.browser.url}>
                in the browser sandbox: {j.browser.title || j.browser.url} (Stop closes it too)
              </div>
            )}
          </CardContent>
        </Card>
      ))}
      {dialog}
    </Page>
  );
}
