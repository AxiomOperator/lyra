// AI usage: every model call lyra makes, by person, kind and model. Admins
// see everyone's total and each person's; members their own.

import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { useCallback, useEffect, useState } from "react";
import { Back, Failed, Page } from "./parts";
import { useLyra } from "./store";

type Total = { calls: number; input: number; cached: number; output: number; ms: number; cost: number };
type UsageData = {
  days: number;
  currency: string;
  priced: boolean;
  total: Total;
  users: { user: string; name: string; total: Total; kinds: Record<string, Total> }[];
  kinds: Record<string, Total>;
  models: Record<string, Total>;
  daily: { day: string; tokens: number; calls: number }[];
  error?: string;
};

const KINDS: Record<string, string> = { chat: "Chat", agent: "Agents & plans", background: "lyra's own work", decision: "Decisions", embedding: "Embeddings", reranker: "Reranking", vision: "Vision" };

function tokens(n: number) {
  return n >= 1e6 ? `${(n / 1e6).toFixed(1)}M` : n >= 1e3 ? `${(n / 1e3).toFixed(1)}k` : String(n);
}

function Line({ t, d }: { t: Total; d: UsageData }) {
  return (
    <span className="text-muted-foreground text-xs tabular-nums">
      {t.calls} calls · {tokens(t.input)} in ({tokens(t.cached)} cached) · {tokens(t.output)} out
      {d.priced && ` · ≈${t.cost.toFixed(2)} ${d.currency}`}
    </span>
  );
}

export function UsagePage({ onBack }: { onBack: () => void }) {
  const { call, ready, user } = useLyra();
  const admin = user?.admin ?? true;
  const [days, setDays] = useState(7);
  const [data, setData] = useState<UsageData | null>(null);
  const load = useCallback(() => void call<UsageData>("usage", { days }).then(setData), [call, days]);
  useEffect(() => {
    if (ready) load();
  }, [ready, load]);
  const max = Math.max(1, ...(data?.daily ?? []).map((x) => x.tokens));
  const most = Math.max(1, ...(data?.users ?? []).map((u) => u.total.input + u.total.output));
  return (
    <Page title="Usage" description={admin ? "AI usage across everyone, and each person's." : "Your AI usage."} action={<Back onBack={onBack} />}>
      <div className="flex gap-1">
        {[1, 7, 30, 90].map((n) => (
          <Button key={n} size="sm" variant={n === days ? "default" : "secondary"} onClick={() => setDays(n)}>
            {n === 1 ? "Today" : `${n} days`}
          </Button>
        ))}
      </div>
      {data?.error && <Failed error={data.error} />}
      {data && !data.error && (
        <>
          <Card className="gap-1 py-3">
            <CardHeader className="px-4">
              <CardTitle className="text-sm">{admin ? "Everyone" : "You"}</CardTitle>
              <CardDescription>
                <Line t={data.total} d={data} />
              </CardDescription>
            </CardHeader>
            <CardContent className="px-4">
              <div className="flex h-24 items-end gap-0.5">
                {data.daily.map((x) => (
                  <div key={x.day} title={`${x.day}: ${tokens(x.tokens)} tokens, ${x.calls} calls`} className="flex-1 rounded-t bg-primary/70" style={{ height: `${Math.max(2, (x.tokens / max) * 100)}%` }} />
                ))}
                {data.daily.length === 0 && <div className="text-muted-foreground text-sm">Nothing yet.</div>}
              </div>
            </CardContent>
          </Card>
          {admin && (
            <Card className="gap-0 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">By person</CardTitle>
                <CardAction className="text-muted-foreground text-xs">{data.users.length}</CardAction>
              </CardHeader>
              <CardContent className="divide-y px-4">
                {data.users.map((u) => (
                  <div key={u.user} className="py-2">
                    <div className="flex items-baseline justify-between gap-2">
                      <span className="text-sm">{u.name}</span>
                      <span className="text-muted-foreground text-xs tabular-nums">{tokens(u.total.input + u.total.output)}</span>
                    </div>
                    <div className="my-1 h-1.5 rounded bg-muted">
                      <div className="h-full rounded bg-primary/70" style={{ width: `${((u.total.input + u.total.output) / most) * 100}%` }} />
                    </div>
                    <Line t={u.total} d={data} />
                    <div className="text-muted-foreground text-xs">
                      {Object.entries(u.kinds)
                        .map(([k, t]) => `${KINDS[k] ?? k} ${tokens(t.input + t.output)}`)
                        .join(" · ")}
                    </div>
                  </div>
                ))}
              </CardContent>
            </Card>
          )}
          <div className="grid gap-3 md:grid-cols-2">
            {(
              [
                ["By kind", data.kinds, (k: string) => KINDS[k] ?? k],
                ["By model", data.models, (k: string) => k],
              ] as const
            ).map(([title, rows, label]) => (
              <Card key={title} className="gap-0 py-3">
                <CardHeader className="px-4">
                  <CardTitle className="text-sm">{title}</CardTitle>
                </CardHeader>
                <CardContent className={cn("divide-y px-4")}>
                  {Object.entries(rows).map(([k, t]) => (
                    <div key={k} className="flex flex-col py-2">
                      <span className="truncate text-sm">{label(k)}</span>
                      <Line t={t} d={data} />
                    </div>
                  ))}
                </CardContent>
              </Card>
            ))}
          </div>
        </>
      )}
    </Page>
  );
}
