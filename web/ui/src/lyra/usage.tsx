// AI usage: every model call lyra makes, by person, kind and model. Admins
// see everyone's total and each person's (a name opens that person: who they
// are, their days, kinds, models and biggest conversations); members their own.

import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { Back, Failed, Page } from "./parts";
import { ago } from "./push";
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
  jobs?: { job: string; title?: string; kind?: string; total: Total }[];
  person?: { id: string; name: string; email: string; role: string; status: string; created: string; last_seen?: string | null; sign_in: string; devices: { name: string; last_seen: string }[] };
  error?: string;
};

const JOB_KINDS: Record<string, string> = { chat: "chat", diagnosis: "diagnosis", routine: "routine" };

/** One person, for an admin: who they are. */
function PersonCard({ p }: { p: NonNullable<UsageData["person"]> }) {
  const row = (k: string, v: string) => (
    <div className="flex justify-between gap-3 py-1 text-sm">
      <span className="text-muted-foreground">{k}</span>
      <span className="min-w-0 truncate text-right">{v}</span>
    </div>
  );
  return (
    <Card className="gap-0 py-3">
      <CardHeader className="px-4">
        <CardTitle className="text-sm">{p.name}</CardTitle>
        <CardDescription>{p.email || "no email"}</CardDescription>
      </CardHeader>
      <CardContent className="divide-y px-4">
        {row("Role", p.role === "admin" ? "Admin" : "Member")}
        {row("Account", p.status.charAt(0).toUpperCase() + p.status.slice(1))}
        {row("Signs in with", p.sign_in)}
        {row("Last seen", p.last_seen ? ago(p.last_seen) : "never")}
        {row("Joined", new Date(p.created).toLocaleDateString())}
        {row(
          "Devices",
          p.devices.length
            ? `${p.devices.length} · latest: ${[...p.devices]
                .sort((a, b) => b.last_seen.localeCompare(a.last_seen))
                .slice(0, 3)
                .map((d) => `${d.name} (${ago(d.last_seen)})`)
                .join(", ")}`
            : "none",
        )}
      </CardContent>
    </Card>
  );
}

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
  // An admin looking at one person (null: everyone).
  const [person, setPerson] = useState<{ id: string; name: string } | null>(null);
  const load = useCallback(() => void call<UsageData>("usage", { days, user: person?.id ?? "" }).then(setData), [call, days, person]);
  useEffect(() => {
    if (ready) load();
  }, [ready, load]);
  const max = Math.max(1, ...(data?.daily ?? []).map((x) => x.tokens));
  const most = Math.max(1, ...(data?.users ?? []).map((u) => u.total.input + u.total.output));
  return (
    <Page
      title={person ? `Usage · ${person.name}` : "Usage"}
      description={person ? `${person.name}'s AI usage.` : admin ? "AI usage across everyone, and each person's (choose a name for more)." : "Your AI usage."}
      action={
        person ? (
          <Button size="sm" variant="ghost" onClick={() => setPerson(null)}>
            <ChevronLeft /> Everyone
          </Button>
        ) : (
          <Back onBack={onBack} />
        )
      }
    >
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
          {person && data.person && <PersonCard p={data.person} />}
          <Card className="gap-1 py-3">
            <CardHeader className="px-4">
              <CardTitle className="text-sm">{person ? person.name : admin ? "Everyone" : "You"}</CardTitle>
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
          {admin && !person && (
            <Card className="gap-0 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">By person</CardTitle>
                <CardAction className="text-muted-foreground text-xs">{data.users.length}</CardAction>
              </CardHeader>
              <CardContent className="divide-y px-4">
                {data.users.map((u) => (
                  <button key={u.user} type="button" onClick={() => setPerson({ id: u.user, name: u.name })} className="-mx-2 block w-[calc(100%+1rem)] rounded-md px-2 py-2 text-left hover:bg-muted/50">
                    <div className="flex items-baseline justify-between gap-2">
                      <span className="text-sm underline-offset-2 hover:underline">{u.name}</span>
                      <span className="flex items-center gap-1 text-muted-foreground text-xs tabular-nums">
                        {tokens(u.total.input + u.total.output)}
                        <ChevronRight className="size-3.5" />
                      </span>
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
                  </button>
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
          {(data.jobs?.length ?? 0) > 0 && (
            <Card className="gap-0 py-3">
              <CardHeader className="px-4">
                <CardTitle className="text-sm">Biggest conversations</CardTitle>
                <CardDescription>Chats, diagnoses and routine runs that used the most.</CardDescription>
              </CardHeader>
              <CardContent className="divide-y px-4">
                {data.jobs!.map((j) => (
                  <div key={j.job} className="flex flex-col py-2">
                    <span className="flex items-baseline gap-2 text-sm">
                      <span className="min-w-0 truncate">{j.title || "(untitled)"}</span>
                      {j.kind && j.kind !== "chat" && <span className="shrink-0 text-muted-foreground text-xs">{JOB_KINDS[j.kind] ?? j.kind}</span>}
                    </span>
                    <Line t={j.total} d={data} />
                  </div>
                ))}
              </CardContent>
            </Card>
          )}
        </>
      )}
    </Page>
  );
}
