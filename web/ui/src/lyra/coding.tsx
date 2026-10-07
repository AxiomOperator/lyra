// Coding work handed to Claude Code / OpenCode: a live card while it runs
// (which agent, why, its steps), then what it changed — files, diff stat,
// commits, a hand-over if OpenCode couldn't finish — with Show diff,
// Continue and Open in….

import { MessageResponse } from "@/components/ai-elements/message";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { ArrowRight, Code2, FileDiff, Loader2, Play, Terminal } from "lucide-react";
import { useState } from "react";
import { Page } from "./parts";
import { ago } from "./push";
import { useData, useLyra } from "./store";

const title = (h?: string) => (h === "claude" ? "Claude Code" : h === "opencode" ? "OpenCode" : h ?? "coding agent");

interface Progress {
  kind: "start" | "harness" | "tool" | "text" | "handover" | "error";
  text?: string;
  session?: string;
  model?: string;
}

interface CodeResult {
  ok?: boolean;
  error?: string;
  harness?: string;
  machine?: string;
  dir?: string;
  mode?: string;
  why?: string;
  session?: string;
  model?: string;
  summary?: string;
  files?: string[];
  diff_stat?: string;
  commits?: string[];
  seconds?: number;
  turns?: number;
  cost_usd?: number | null;
  handed_over_from?: { harness: string; error?: string; summary?: string };
  progress?: Progress[];
}

/** `code_task` under its agent card: live steps, then the result. */
export function CodingCard({ args, result }: { args: Record<string, unknown>; result?: string }) {
  const { call, say } = useLyra();
  const [diff, setDiff] = useState<string | null>(null);
  const [open, setOpen] = useState(false);
  let r: CodeResult = {};
  try {
    r = result ? (JSON.parse(result) as CodeResult) : {};
  } catch {
    r = { error: result };
  }
  const live = !result || Array.isArray(r.progress);
  const steps = r.progress ?? [];
  const head = steps.filter((e) => e.kind === "harness" || e.kind === "handover");
  const dir = (r.dir ?? args.dir) as string | undefined;
  const machine = (r.machine ?? args.machine ?? "") as string;
  const resume = r.harness === "claude" ? `cd ${dir} && claude --resume ${r.session}` : `cd ${dir} && opencode -s ${r.session}`;
  return (
    <div className="space-y-2 rounded-lg border bg-background/40 p-3 text-sm">
      <div className="flex flex-wrap items-center gap-2">
        <Code2 className="size-4 text-violet-300" />
        <span className="font-medium">{live ? head.at(-1)?.text ?? "Picking a coding agent…" : `${title(r.harness)}${machine ? ` on ${machine}` : ""}`}</span>
        <span className="truncate text-muted-foreground text-xs">{dir}</span>
        {live ? (
          <Badge variant="outline" className="ml-auto text-sky-300">
            <Loader2 className="animate-spin" /> working
          </Badge>
        ) : (
          <Badge variant="outline" className={cn("ml-auto", r.ok ? "text-emerald-300" : "text-red-300")}>
            {r.ok ? "done" : "didn't finish"}
          </Badge>
        )}
      </div>
      <p className="text-muted-foreground text-xs">{String(args.task ?? "").slice(0, 300)}</p>
      {live && (
        <ul className="max-h-48 space-y-0.5 overflow-y-auto font-mono text-[11px]">
          {steps
            .filter((e) => e.kind === "tool" || e.kind === "error" || e.kind === "handover")
            .slice(-12)
            .map((e, i) => (
              <li key={i} className={cn(e.kind === "error" ? "text-red-300" : e.kind === "handover" ? "text-amber-300" : "text-muted-foreground")}>
                {e.kind === "handover" ? "↪ " : "· "}
                {e.text}
              </li>
            ))}
        </ul>
      )}
      {!live && (
        <>
          {r.why && <p className="text-muted-foreground text-xs">{r.why}</p>}
          {r.handed_over_from && (
            <p className="flex items-center gap-1.5 text-amber-300 text-xs">
              {title(r.handed_over_from.harness)} couldn't finish <ArrowRight className="size-3" /> {title(r.harness)} took over
            </p>
          )}
          {(r.summary || r.error) && (
            <div className="rounded-md bg-muted/30 p-2 text-xs">
              <MessageResponse>{r.summary || r.error || ""}</MessageResponse>
            </div>
          )}
          <div className="flex flex-wrap items-center gap-x-3 gap-y-1 text-muted-foreground text-xs">
            {!!r.files?.length && (
              <button type="button" onClick={() => setOpen(!open)} className="hover:text-foreground">
                {r.files.length} file{r.files.length === 1 ? "" : "s"} changed {open ? "▾" : "▸"}
              </button>
            )}
            {r.diff_stat && <span>{r.diff_stat}</span>}
            {!!r.commits?.length && <span>{r.commits.length} local commit{r.commits.length === 1 ? "" : "s"}</span>}
            {r.seconds !== undefined && <span>{Math.round(r.seconds / 60) || "<1"} min</span>}
            {!!r.cost_usd && <span>${r.cost_usd.toFixed(2)}</span>}
          </div>
          {open && (
            <ul className="font-mono text-[11px] text-muted-foreground">
              {r.files?.map((f) => <li key={f}>{f}</li>)}
              {r.commits?.map((c) => <li key={c} className="text-violet-300">{c}</li>)}
            </ul>
          )}
          {diff !== null && <pre className="max-h-96 overflow-auto rounded-md bg-black/40 p-2 font-mono text-[11px] whitespace-pre">{diff || "no changes"}</pre>}
          <div className="flex flex-wrap gap-2">
            {dir && (
              <Button
                size="sm"
                variant="secondary"
                className="h-7 text-xs"
                onClick={async () => {
                  if (diff !== null) return setDiff(null);
                  const d = await call<{ diff?: string; error?: string }>("coding_diff", { machine: machine || "server", dir });
                  setDiff(d.diff ?? d.error ?? "");
                }}
              >
                <FileDiff /> {diff !== null ? "Hide diff" : "Show diff"}
              </Button>
            )}
            {r.session && (
              <Button size="sm" variant="ghost" className="h-7 text-xs" onClick={() => window.dispatchEvent(new CustomEvent("lyra-prefill", { detail: `Continue the coding job in ${dir}${machine ? ` on @${machine}` : ""}: ` }))}>
                <Play /> Continue
              </Button>
            )}
            {r.session && (
              <Button size="sm" variant="ghost" className="h-7 text-xs" title={resume} onClick={() => void navigator.clipboard?.writeText(resume)}>
                <Terminal /> Copy resume command
              </Button>
            )}
            {!r.ok && r.files?.length ? (
              <Button size="sm" variant="ghost" className="h-7 text-xs text-red-300" onClick={() => say(`Undo the uncommitted changes the coding job left in ${dir}${machine ? ` on @${machine}` : ""}: ${r.files?.join(", ")}`)}>
                Revert…
              </Button>
            ) : null}
          </div>
        </>
      )}
    </div>
  );
}

interface Job {
  at: string;
  machine: string;
  dir: string;
  harness: string;
  why: string;
  task: string;
  session: string | null;
  ok: boolean;
  summary: string;
  files: string[];
  diff_stat: string;
  handed_over: boolean;
}

/** More → Coding: past jobs. */
export function CodingPage({ onBack }: { onBack: () => void }) {
  const [jobs] = useData<Job[]>("coding");
  const { status } = useLyra();
  const machines = (status.machines_detail ?? []).filter((m) => m.online && m.harnesses && Object.keys(m.harnesses).length);
  return (
    <Page
      title="Coding"
      description="Work lyra handed to Claude Code (complex) and OpenCode (simple, with Claude Code as backup)."
      action={
        <Button size="sm" variant="ghost" className="md:hidden" onClick={onBack}>
          Back
        </Button>
      }
    >
      <Card className="py-4">
        <CardHeader className="px-4">
          <CardTitle className="text-base">Coding agents</CardTitle>
          <CardDescription>Ask in chat: "fix the failing test in ~/Projects/foo on @desktop", "have Claude Code refactor …".</CardDescription>
        </CardHeader>
        <CardContent className="flex flex-wrap gap-2 px-4">
          {machines.length === 0 && <span className="text-muted-foreground text-sm">No connected machine has Claude Code or OpenCode.</span>}
          {machines.map((m) =>
            Object.entries(m.harnesses ?? {}).map(([h, v]) => (
              <Badge key={m.name + h} variant="outline">
                {title(h)} {String(v).split(" ")[0]} · {m.name}
              </Badge>
            )),
          )}
        </CardContent>
      </Card>
      {(jobs ?? []).length === 0 && <p className="text-muted-foreground text-sm">No coding jobs yet.</p>}
      {(jobs ?? []).map((j) => (
        <Card key={j.at + j.dir} className="gap-2 py-3">
          <CardHeader className="px-4">
            <CardTitle className="flex flex-wrap items-center gap-2 text-sm">
              {title(j.harness)}
              <span className="font-normal text-muted-foreground text-xs">
                {j.machine} · {j.dir}
              </span>
              <Badge variant="outline" className={cn("ml-auto", j.ok ? "text-emerald-300" : "text-red-300")}>
                {j.ok ? "done" : "didn't finish"}
              </Badge>
            </CardTitle>
            <CardDescription className="break-words">{j.task}</CardDescription>
          </CardHeader>
          <CardContent className="space-y-1 px-4 text-muted-foreground text-xs">
            <p>
              {ago(j.at)} · {j.why}
              {j.files.length ? ` · ${j.files.length} files · ${j.diff_stat}` : ""}
            </p>
            {j.summary && <p className="line-clamp-3 text-foreground/80">{j.summary}</p>}
          </CardContent>
        </Card>
      ))}
    </Page>
  );
}
