// Proposals (admins): what lyra's evolution proposes — new APIs from their
// OpenAPI descriptions, tools, workflows, settings, code patches — waiting
// for a decision. Each shows what it is and why; Approve puts it in place
// (an API goes live, its writes still asking each time), Test checks it
// first where it needs that, Reject drops it. Changes go through /evolve.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Check, FlaskConical, Globe, KeyRound, Plus, X } from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { Back, Failed, Page, useAction } from "./parts";
import { ago } from "./push";
import { useLyra } from "./store";

interface ApiInfo {
  name: string;
  title: string;
  spec: string;
  reads: number;
  writes: number;
  sample: string[];
  key: string | null;
  key_saved: boolean;
}

interface Proposal {
  id: string;
  level: string;
  status: string;
  problem: string;
  rationale: string;
  change: string;
  confidence: number;
  created: string;
  tested: boolean;
  valid: boolean;
  notes: string[];
  api: ApiInfo | null;
  code: boolean;
}

function ProposalCard({ p, act, busy }: { p: Proposal; act: (c: string) => Promise<boolean>; busy: boolean }) {
  const ready = p.tested && p.valid;
  return (
    <Card className="gap-2 py-3">
      <CardHeader className="px-4">
        <CardTitle className="flex flex-wrap items-center gap-2 text-sm">
          {p.api ? <Globe className="size-4 text-sky-400" /> : null}
          {p.api ? `API: ${p.api.title || p.api.name}` : p.change}
          <Badge variant="outline">{p.api ? "new API" : p.level}</Badge>
          {p.tested && <Badge variant="secondary" className={cn(!p.valid && "bg-red-500/20 text-red-300")}>{p.valid ? "checked" : "failed its checks"}</Badge>}
        </CardTitle>
        <CardDescription className="break-words">{p.problem}</CardDescription>
        <CardAction className="text-muted-foreground text-xs">{ago(p.created)}</CardAction>
      </CardHeader>
      <CardContent className="space-y-2 px-4 text-sm">
        <p className="text-muted-foreground">{p.rationale}</p>
        {p.api && (
          <div className="space-y-1 rounded-md border px-3 py-2">
            <div className="text-xs">
              <span className="font-medium">{p.api.reads}</span> read and <span className="font-medium">{p.api.writes}</span> write operations, as <span className="font-mono">{p.api.name}.…</span> · writes ask each time
            </div>
            <div className="truncate text-muted-foreground text-xs" title={p.api.spec}>
              from {p.api.spec}
            </div>
            {p.api.key && (
              <div className={cn("flex items-center gap-1.5 text-xs", p.api.key_saved ? "text-emerald-300" : "text-amber-300")}>
                <KeyRound className="size-3.5" />
                {p.api.key_saved ? `its key is saved (${p.api.key})` : `needs a key: lyra secret ${p.api.key} on the server`}
              </div>
            )}
            {p.api.sample.length > 0 && (
              <ul className="list-inside list-disc text-muted-foreground text-xs">
                {p.api.sample.map((s) => (
                  <li key={s} className="truncate">
                    {s}
                  </li>
                ))}
              </ul>
            )}
          </div>
        )}
        {!p.api && <p className="break-words font-mono text-xs">{p.change}</p>}
        {p.notes.length > 0 && <p className="text-muted-foreground text-xs">{p.notes.join(" · ")}</p>}
        <div className="flex flex-wrap gap-2">
          <Button size="sm" disabled={busy || !ready} onClick={() => void act(`/evolve approve ${p.id}`)} title={ready ? (p.code ? "Commits the patch to a local branch (never merged or pushed)" : "Put it in place") : "Test it first"}>
            <Check /> Approve
          </Button>
          {!ready && (
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => void act(`/evolve test ${p.id}`)}>
              <FlaskConical /> Test
            </Button>
          )}
          <Button size="sm" variant="ghost" className="text-red-400 hover:text-red-300" disabled={busy} onClick={() => void act(`/evolve reject ${p.id}`)}>
            <X /> Reject
          </Button>
        </div>
      </CardContent>
    </Card>
  );
}

export function ProposalsPage({ onBack }: { onBack: () => void }) {
  const { call, ready } = useLyra();
  const [list, setList] = useState<Proposal[] | { error: string } | null>(null);
  const load = useCallback(() => void call<Proposal[] | { error: string }>("evolution_candidates").then(setList), [call]);
  // Approving and testing run in the background: look again a little later too.
  const { act, busy, note } = useAction(() => {
    load();
    window.setTimeout(load, 4000);
    window.setTimeout(load, 12000);
  });
  const [url, setUrl] = useState("");
  useEffect(() => {
    if (ready) load();
  }, [ready, load]);
  const propose = async (e: FormEvent) => {
    e.preventDefault();
    if (url.trim() && (await act(`/evolve api ${url.trim()}`))) setUrl("");
  };
  const items = Array.isArray(list) ? list : [];
  return (
    <Page title="Proposals" description="What lyra proposes to add or change: new APIs, tools, workflows and settings. Nothing changes until you approve it." action={<Back onBack={onBack} />}>
      {list && !Array.isArray(list) && <Failed error={list.error} />}
      {note}
      <form onSubmit={propose} className="flex gap-2">
        <Input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="Add an API: its OpenAPI description's URL (https://…/openapi.json)" aria-label="An OpenAPI description's URL" />
        <Button type="submit" variant="secondary" disabled={busy || !url.trim()}>
          <Plus /> Propose
        </Button>
      </form>
      {list && Array.isArray(list) && !items.length && <p className="text-muted-foreground text-sm">Nothing waiting for a decision.</p>}
      {items.map((p) => (
        <ProposalCard key={p.id} p={p} act={act} busy={busy} />
      ))}
    </Page>
  );
}
