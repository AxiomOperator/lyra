// More of the chat's AI Elements: the plan this conversation runs (Plan,
// Queue, Checkpoint), command output as a terminal (Terminal), folder
// listings as a tree (File tree), file contents with highlighting (Code
// block), and switching models from the composer (Model selector).

import { Checkpoint, CheckpointIcon, CheckpointTrigger } from "@/components/ai-elements/checkpoint";
import { CodeBlock } from "@/components/ai-elements/code-block";
import { FileTree, FileTreeFile, FileTreeFolder } from "@/components/ai-elements/file-tree";
import { ModelSelector, ModelSelectorContent, ModelSelectorEmpty, ModelSelectorGroup, ModelSelectorInput, ModelSelectorItem, ModelSelectorList, ModelSelectorTrigger } from "@/components/ai-elements/model-selector";
import { Plan, PlanAction, PlanContent, PlanDescription, PlanFooter, PlanHeader, PlanTitle, PlanTrigger } from "@/components/ai-elements/plan";
import { QueueItem, QueueItemContent, QueueItemDescription, QueueItemIndicator, QueueList, QueueSection, QueueSectionContent, QueueSectionLabel, QueueSectionTrigger } from "@/components/ai-elements/queue";
import { Terminal } from "@/components/ai-elements/terminal";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { CheckCircle2, ChevronDown, CircleDashed, ListTodo, Loader2, OctagonX, Pause } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import type { BundledLanguage } from "shiki";
import { useLyra } from "./store";

// ---- the plan

export interface PlanStepView {
  key: string;
  title: string;
  description: string;
  status: "pending" | "running" | "completed" | "failed" | "blocked" | "skipped" | "cancelled";
  needs_approval: boolean;
  error: string | null;
  attempts: number;
}

export interface PlanView {
  id: string;
  version: number;
  status: string;
  busy: boolean;
  goal: string | null;
  steps: PlanStepView[];
  budget: string;
  note: string | null;
}

interface CheckpointView {
  at: string;
  version: number;
  reason: string;
  done: string[];
}

const DONE = new Set(["completed", "skipped"]);

function StepRow({ s }: { s: PlanStepView }) {
  const done = DONE.has(s.status);
  const icon =
    s.status === "running" ? <Loader2 className="size-3.5 animate-spin text-sky-400" /> : s.status === "failed" ? <OctagonX className="size-3.5 text-red-400" /> : s.status === "blocked" ? <Pause className="size-3.5 text-amber-400" /> : null;
  return (
    <QueueItem>
      <div className="flex items-center gap-2">
        {icon ?? <QueueItemIndicator completed={done} />}
        <QueueItemContent completed={done}>
          <span className="font-mono text-muted-foreground">{s.key}</span> {s.title}
          {s.needs_approval && !done && <span className="ml-1 text-amber-400">⚠</span>}
        </QueueItemContent>
      </div>
      {(s.error || (s.status === "running" && s.description)) && (
        <QueueItemDescription completed={done} className={cn(s.error && "text-red-300")}>
          {s.error ?? s.description}
          {s.attempts > 1 && ` (try ${s.attempts})`}
        </QueueItemDescription>
      )}
    </QueueItem>
  );
}

/** The conversation's plan: what's next, what's done, its recovery points. */
export function PlanCard({ plan }: { plan: PlanView }) {
  const { run, call } = useLyra();
  const [checkpoints, setCheckpoints] = useState<CheckpointView[] | null>(null);
  const [note, setNote] = useState("");
  const live = !["completed", "cancelled", "failed"].includes(plan.status);
  const todo = plan.steps.filter((s) => !DONE.has(s.status));
  const done = plan.steps.filter((s) => DONE.has(s.status));
  const act = async (command: string) => {
    const r = await run(command);
    setNote(r.text);
  };
  const loadCheckpoints = useCallback(() => void call<CheckpointView[] | { error: string }>("plan_checkpoints").then((c) => setCheckpoints(Array.isArray(c) ? c : [])), [call]);
  // Fresh recovery points as steps finish.
  useEffect(() => {
    if (checkpoints) loadCheckpoints();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [done.length]);
  return (
    <Plan isStreaming={plan.busy} defaultOpen={live} className="mx-3 mb-2">
      <PlanHeader>
        <div className="min-w-0">
          <PlanTitle>{plan.goal || `Plan ${plan.id}`}</PlanTitle>
          <PlanDescription>{`Plan ${plan.id} · v${plan.version} · ${plan.status}${plan.busy ? " · working…" : ""} · ${done.length}/${plan.steps.length} done`}</PlanDescription>
        </div>
        <PlanAction>
          <PlanTrigger />
        </PlanAction>
      </PlanHeader>
      <PlanContent className="space-y-2">
        {todo.length > 0 && (
          <QueueSection>
            <QueueSectionTrigger>
              <QueueSectionLabel count={todo.length} label="to do" icon={<CircleDashed className="size-4" />} />
            </QueueSectionTrigger>
            <QueueSectionContent>
              <QueueList>
                {todo.map((s) => (
                  <StepRow key={s.key} s={s} />
                ))}
              </QueueList>
            </QueueSectionContent>
          </QueueSection>
        )}
        {done.length > 0 && (
          <QueueSection defaultOpen={false}>
            <QueueSectionTrigger>
              <QueueSectionLabel count={done.length} label="done" icon={<CheckCircle2 className="size-4" />} />
            </QueueSectionTrigger>
            <QueueSectionContent>
              <QueueList>
                {done.map((s) => (
                  <StepRow key={s.key} s={s} />
                ))}
              </QueueList>
            </QueueSectionContent>
          </QueueSection>
        )}
        {plan.note && <p className="text-amber-300 text-sm">{plan.note}</p>}
        <p className="text-muted-foreground text-xs">{plan.budget}</p>
        {/* Recovery points: saved before steps that change things. */}
        <div className="space-y-1">
          {checkpoints === null ? (
            <Button size="sm" variant="ghost" className="h-7 px-2 text-xs" onClick={loadCheckpoints}>
              <ChevronDown className="size-3.5" /> Recovery points
            </Button>
          ) : checkpoints.length === 0 ? (
            <p className="text-muted-foreground text-xs">No recovery points yet: they're saved before steps that change things.</p>
          ) : (
            checkpoints.map((c, i) => (
              <Checkpoint key={c.at}>
                <CheckpointIcon />
                <span className="shrink-0 text-xs">
                  {new Date(c.at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })} · v{c.version} · {c.reason}
                  {c.done.length > 0 && ` · done: ${c.done.join(", ")}`}
                </span>
                {i === 0 && live && !plan.busy && (
                  <CheckpointTrigger tooltip="Carry on from the latest recovery point" onClick={() => void act(`/plan resume ${plan.id}`)}>
                    Resume
                  </CheckpointTrigger>
                )}
              </Checkpoint>
            ))
          )}
        </div>
        {note && <p className="whitespace-pre-wrap text-muted-foreground text-xs">{note}</p>}
      </PlanContent>
      {live && (
        <PlanFooter className="flex justify-end gap-2">
          {!plan.busy && (
            <Button size="sm" variant="secondary" onClick={() => void act(`/plan run ${plan.id}`)}>
              <ListTodo /> Run
            </Button>
          )}
          <Button size="sm" variant="ghost" onClick={() => void act(`/plan cancel ${plan.id}`)}>
            Cancel plan
          </Button>
        </PlanFooter>
      )}
    </Plan>
  );
}

// ---- command output

/** A command's output (stdout, then stderr) as a terminal. */
export function ToolTerminal({ title, stdout, stderr, exit, seconds, timedOut }: { title: string; stdout: string; stderr: string; exit: unknown; seconds?: number; timedOut?: boolean }) {
  const output = [stdout, stderr && `\u001b[31m${stderr}\u001b[0m`].filter(Boolean).join(stdout && stderr ? "\n" : "");
  return (
    <div className="space-y-1">
      <h4 className="font-medium text-muted-foreground text-xs uppercase tracking-wide">
        {title} · exit {String(exit ?? "?")}
        {timedOut ? " · timed out" : ""}
        {typeof seconds === "number" ? ` · ${seconds}s` : ""}
      </h4>
      <Terminal output={output || "(no output)"} className="max-h-80 text-xs [&_pre]:whitespace-pre-wrap [&_pre]:break-words" />
    </div>
  );
}

// ---- folder listings

interface Entry {
  path: string;
  dir?: boolean;
  kind?: string;
  size?: number;
}

type Node = { name: string; path: string; dir: boolean; size?: number; children: Map<string, Node> };

function tree(entries: Entry[]): Node {
  const root: Node = { name: "", path: "", dir: true, children: new Map() };
  for (const e of entries) {
    const parts = e.path.replace(/\\/g, "/").split("/").filter(Boolean);
    let at = root;
    parts.forEach((p, i) => {
      const path = parts.slice(0, i + 1).join("/");
      const last = i === parts.length - 1;
      let n = at.children.get(p);
      if (!n) {
        n = { name: p, path, dir: !last || !!e.dir || e.kind === "dir", size: last ? e.size : undefined, children: new Map() };
        at.children.set(p, n);
      } else if (last && (e.dir || e.kind === "dir")) n.dir = true;
      at = n;
    });
  }
  return root;
}

function size(n?: number) {
  if (n === undefined) return "";
  return n >= 1048576 ? `${(n / 1048576).toFixed(1)} MB` : n >= 1024 ? `${Math.round(n / 1024)} KB` : `${n} B`;
}

function NodeView({ n }: { n: Node }) {
  const kids = [...n.children.values()].sort((a, b) => Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name));
  return n.dir ? (
    <FileTreeFolder path={n.path} name={n.name}>
      {kids.map((k) => (
        <NodeView key={k.path} n={k} />
      ))}
    </FileTreeFolder>
  ) : (
    <FileTreeFile path={n.path} name={n.name}>
      <span className="flex w-full items-center gap-2">
        <span className="truncate">{n.name}</span>
        <span className="ml-auto shrink-0 text-muted-foreground text-xs">{size(n.size)}</span>
      </span>
    </FileTreeFile>
  );
}

/** A folder listing (project_list, file_list) as a tree. */
export function ToolFileTree({ entries, cut }: { entries: Entry[]; cut?: boolean }) {
  const root = tree(entries);
  const top = [...root.children.values()].sort((a, b) => Number(b.dir) - Number(a.dir) || a.name.localeCompare(b.name));
  // The first level open, so the listing reads at a glance.
  const open = new Set(top.filter((n) => n.dir).map((n) => n.path));
  return (
    <div className="space-y-1">
      <FileTree defaultExpanded={open} className="max-h-80 overflow-auto text-sm">
        {top.map((n) => (
          <NodeView key={n.path} n={n} />
        ))}
      </FileTree>
      {cut && <p className="text-muted-foreground text-xs">Only the first entries are shown.</p>}
    </div>
  );
}

// ---- file contents

const LANGS: Record<string, BundledLanguage> = {
  rs: "rust", ts: "typescript", tsx: "tsx", js: "javascript", jsx: "jsx", py: "python", sh: "bash", bash: "bash", ps1: "powershell", json: "json", toml: "toml",
  yaml: "yaml", yml: "yaml", md: "markdown", html: "html", css: "css", sql: "sql", go: "go", java: "java", c: "c", h: "c", cpp: "cpp", cs: "csharp", xml: "xml",
  ini: "ini", conf: "ini", dockerfile: "docker", rb: "ruby", php: "php", lua: "lua", nix: "nix", csv: "csv", log: "log",
};

/** A file's language for highlighting, by its name. */
export function languageOf(path: string): BundledLanguage | null {
  const name = path.split(/[\\/]/).pop()?.toLowerCase() ?? "";
  if (name === "dockerfile") return "docker";
  return LANGS[name.split(".").pop() ?? ""] ?? null;
}

/** What a file says (file_read, project_read), highlighted when it's code. */
export function ToolFileContent({ path, text, cut }: { path: string; text: string; cut?: boolean }) {
  const lang = languageOf(path);
  return (
    <div className="space-y-1">
      {lang ? (
        <CodeBlock code={text} language={lang} className="max-h-96 overflow-auto text-xs" />
      ) : (
        <pre className="max-h-80 overflow-auto rounded-md bg-black/40 p-3 font-mono text-xs whitespace-pre-wrap break-words">{text}</pre>
      )}
      {cut && <p className="text-muted-foreground text-xs">Only the start of the file.</p>}
    </div>
  );
}

// ---- the model

/** The model in the composer: tap to pick another (admins; it applies to everyone). */
export function ComposerModel({ admin }: { admin: boolean }) {
  const { status, call, run } = useLyra();
  const [open, setOpen] = useState(false);
  const [models, setModels] = useState<string[] | null>(null);
  useEffect(() => {
    if (open && !models) void call<{ models: string[] }>("models").then((d) => setModels(d?.models ?? []));
  }, [open, models, call]);
  if (!admin) return <span className="truncate px-1 text-muted-foreground text-xs">{status.model}</span>;
  return (
    <ModelSelector open={open} onOpenChange={setOpen}>
      <ModelSelectorTrigger asChild>
        <button type="button" className="truncate rounded px-1 text-muted-foreground text-xs hover:text-foreground">
          {status.model}
        </button>
      </ModelSelectorTrigger>
      <ModelSelectorContent title="Switch model">
        <ModelSelectorInput placeholder="Find a model…" />
        <ModelSelectorList>
          <ModelSelectorEmpty>{models === null ? "Asking the server…" : "No models found."}</ModelSelectorEmpty>
          <ModelSelectorGroup heading="On the server (applies to every conversation)">
            {(models ?? []).map((m) => (
              <ModelSelectorItem
                key={m}
                value={m}
                onSelect={() => {
                  setOpen(false);
                  if (m !== status.model) void run(`/model ${m}`);
                }}
              >
                <span className="min-w-0 flex-1 truncate font-mono text-sm">{m}</span>
                {m === status.model && <span className="ml-auto text-teal-400 text-xs">in use</span>}
              </ModelSelectorItem>
            ))}
          </ModelSelectorGroup>
        </ModelSelectorList>
      </ModelSelectorContent>
    </ModelSelector>
  );
}
