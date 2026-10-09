// Running now (admins): every routine, problem write-up, plan and reply at
// work, anyone's, with how long it's run, what it's doing, what it has cost
// so far, and Stop.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Square } from "lucide-react";
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
}

const KIND: Record<Job["kind"], string> = { routine: "routine", diagnosis: "problem write-up", plan: "plan", reply: "reply" };

function tokens(n: number) {
  return n >= 1_000_000 ? `${(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${Math.round(n / 1000)}k` : `${n}`;
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
    <Page title="Running now" description="Everything lyra is working on, for everyone: how long, what it's doing, what it has cost so far." action={<Back onBack={onBack} />}>
      {said && <p className="text-muted-foreground text-sm">{said}</p>}
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
          </CardContent>
        </Card>
      ))}
      {dialog}
    </Page>
  );
}
