// Proposals (admins): what lyra's evolution proposes — new APIs from their
// OpenAPI descriptions, tools, workflows, settings, code patches — waiting
// for a decision. Each shows what it is and why; Approve puts it in place
// (an API goes live, its writes still asking each time), Test checks it
// first where it needs that, Reject drops it. Changes go through /evolve.
// A code patch shows the test that reproduces its problem, the fix, and each
// sandbox step (the test failing before the fix, passing with it, clippy and
// every test); approved, it's a local branch listed under Branches, for a
// person to review and merge (lyra never merges or pushes).

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Check, CircleCheck, CircleX, FlaskConical, GitBranch, Globe, KeyRound, Plus, X } from "lucide-react";
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
  patch: Patch | null;
}

interface Patch {
  base: string;
  test: string;
  diff: string;
  tests: string[];
  checks: { name: string; ok: boolean; output: string }[];
}

interface Branch extends Patch {
  id: string;
  problem: string;
  rationale: string;
  branch: string;
  at: string;
  there: boolean;
  repo: string | null;
}

/** A diff, coloured: what's taken out and what's put in. */
function Diff({ text }: { text: string }) {
  return (
    <pre className="max-h-80 overflow-auto rounded-md border bg-muted/30 p-2 text-xs leading-relaxed">
      {text.split("\n").map((l, i) => (
        <div key={i} className={cn("whitespace-pre-wrap", l.startsWith("+") && !l.startsWith("+++") && "text-emerald-300", l.startsWith("-") && !l.startsWith("---") && "text-red-300", l.startsWith("@@") && "text-sky-300")}>
          {l || " "}
        </div>
      ))}
    </pre>
  );
}

/** A code patch: its repro test, its fix, and what the sandbox found. */
function PatchView({ p }: { p: Patch }) {
  const [open, setOpen] = useState<"" | "test" | "fix">("");
  return (
    <div className="space-y-2">
      {p.checks.length > 0 && (
        <ul className="space-y-1 text-xs">
          {p.checks.map((c) => (
            <li key={c.name}>
              <span className={cn("flex items-center gap-1.5", c.ok ? "text-emerald-300" : "text-red-300")}>
                {c.ok ? <CircleCheck className="size-3.5" /> : <CircleX className="size-3.5" />} {c.name}
              </span>
              {!c.ok && c.output && <pre className="mt-1 max-h-40 overflow-auto rounded border bg-muted/30 p-1.5 text-[11px]">{c.output}</pre>}
            </li>
          ))}
        </ul>
      )}
      <div className="flex flex-wrap gap-2">
        {p.test && (
          <Button size="sm" variant={open === "test" ? "secondary" : "outline"} onClick={() => setOpen(open === "test" ? "" : "test")}>
            The test that reproduces it{p.tests.length ? ` (${p.tests.join(", ")})` : ""}
          </Button>
        )}
        <Button size="sm" variant={open === "fix" ? "secondary" : "outline"} onClick={() => setOpen(open === "fix" ? "" : "fix")}>
          The fix
        </Button>
        <span className="self-center text-muted-foreground text-xs">against {p.base}</span>
      </div>
      {open === "test" && <Diff text={p.test} />}
      {open === "fix" && <Diff text={p.diff} />}
      {!p.test && <p className="text-amber-300 text-xs">No test reproduces the problem: it can't pass its checks. Propose it again to get one.</p>}
    </div>
  );
}

/** Approved code changes, each on its own local branch, for a person to merge. */
function Branches() {
  const { call, ready } = useLyra();
  const [list, setList] = useState<Branch[] | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  useEffect(() => {
    if (ready) void call<Branch[]>("evolution_branches").then((r) => setList(Array.isArray(r) ? r : []));
  }, [ready, call]);
  if (!list?.length) return null;
  return (
    <Card className="gap-2 py-3">
      <CardHeader className="px-4">
        <CardTitle className="flex items-center gap-2 text-sm">
          <GitBranch className="size-4" /> Branches
        </CardTitle>
        <CardDescription>Code changes you approved: each is a local branch in lyra's source. Review it and merge it yourself; lyra never merges, pushes or changes the running binary.</CardDescription>
      </CardHeader>
      <CardContent className="divide-y divide-border px-4 text-sm">
        {list.map((b) => (
          <div key={b.id} className="space-y-2 py-2">
            <button type="button" className="flex w-full flex-wrap items-center gap-2 text-left" onClick={() => setOpen(open === b.id ? null : b.id)}>
              <span className="font-mono text-xs">{b.branch}</span>
              <Badge variant="outline" className={b.there ? "text-emerald-300" : "text-muted-foreground"}>
                {b.there ? "waiting to be merged" : "merged or removed"}
              </Badge>
              <span className="min-w-0 flex-1 truncate text-muted-foreground">{b.problem}</span>
              <span className="text-muted-foreground text-xs">{ago(b.at)}</span>
            </button>
            {open === b.id && (
              <div className="space-y-2">
                <p className="text-muted-foreground text-xs">{b.rationale}</p>
                {b.there && b.repo && (
                  <pre className="rounded border bg-muted/30 p-1.5 text-[11px]">{`cd ${b.repo}
git diff main...${b.branch}
git merge ${b.branch}`}</pre>
                )}
                <PatchView p={b} />
              </div>
            )}
          </div>
        ))}
      </CardContent>
    </Card>
  );
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
        {p.patch && <PatchView p={p.patch} />}
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
      <Branches />
    </Page>
  );
}
