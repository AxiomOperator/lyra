// Settings → Everything: every setting lyra has (from config.example.toml),
// its value in config.toml now, an editor for its type, and its comment as
// help. Not set means lyra's default applies; Reset takes a setting out of the
// file. Keys are never shown (type a new one to replace it). What decides what
// runs on the server is shown, not changed here; the settings that could lock
// you out ask first. Every save keeps the file as it was (History).
//
// Settings → History: config.toml's versions, newest first: who changed what,
// each change's diff (or from then to now), and putting the file back as it
// was before one.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Check, ChevronDown, ChevronRight, History as HistoryIcon, Lock, RotateCcw, Undo2, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Failed } from "./parts";
import { ago } from "./push";
import { useLyra } from "./store";

type Json = string | number | boolean | null | Json[] | { [k: string]: Json };

interface AnyField {
  path: string;
  key: string;
  kind: "bool" | "int" | "real" | "text" | "list" | "table";
  help: string;
  sample: Json;
  set: boolean;
  value: Json;
  secret?: boolean;
  has_value?: boolean;
  file_only?: string;
  careful?: string;
  at_start?: boolean;
}

interface AnySection {
  name: string;
  title: string;
  help: string;
  fields: AnyField[];
  list: boolean;
  set: boolean;
  file_only?: string;
  entries?: string[];
}

interface AllData {
  sections: AnySection[];
  unknown: string[];
  path?: string;
  error?: string;
}

/** A value as the editor shows it. */
function shown(kind: AnyField["kind"], v: Json): string {
  if (v === null || v === undefined) return "";
  if (kind === "list" && Array.isArray(v)) return v.join(", ");
  if (kind === "table" && typeof v === "object") return `{ ${Object.entries(v as Record<string, Json>).map(([k, x]) => `${k} = ${JSON.stringify(x)}`).join(", ")} }`;
  return String(v);
}

function Row({ f, edit, setEdit }: { f: AnyField; edit: Json | undefined; setEdit: (v: Json | undefined) => void }) {
  const changed = edit !== undefined;
  const value = changed ? edit : f.value;
  const reset = edit === null;
  let editor;
  if (f.file_only) {
    editor = (
      <div className="flex items-center gap-1.5 text-muted-foreground text-xs" title={f.file_only}>
        <Lock className="size-3.5 shrink-0" />
        <span className="max-w-72 truncate font-mono">{f.secret ? (f.has_value ? "set" : "not set") : f.set ? shown(f.kind, f.value) : "not set"}</span>
      </div>
    );
  } else if (f.secret) {
    editor = (
      <Input type="password" autoComplete="off" value={typeof edit === "string" ? edit : ""} onChange={(e) => setEdit(e.target.value || undefined)} placeholder={f.has_value ? "set: type a new one to replace it" : "not set"} className="h-8 w-64" aria-label={f.path} />
    );
  } else if (f.kind === "bool") {
    const on = value === true || (value === null && !reset && f.sample === true);
    editor = (
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={reset ? f.sample === true : !!on} onChange={(e) => setEdit(e.target.checked)} aria-label={f.path} />
        {!f.set && !changed && <span className="text-muted-foreground text-xs">not set</span>}
      </label>
    );
  } else {
    editor = (
      <Input
        value={reset ? "" : shown(f.kind, value)}
        onChange={(e) => setEdit(e.target.value)}
        placeholder={f.set ? "" : `not set${f.sample !== null && f.sample !== "" ? ` (e.g. ${shown(f.kind, f.sample)})` : ""}`}
        inputMode={f.kind === "int" || f.kind === "real" ? "decimal" : undefined}
        className={cn("h-8 w-64 font-mono", f.kind === "list" && "w-80")}
        aria-label={f.path}
      />
    );
  }
  return (
    <div className="flex flex-col gap-1.5 py-2.5 sm:flex-row sm:items-start sm:justify-between">
      <div className="min-w-0 sm:max-w-[55%]">
        <div className={cn("font-mono text-sm", changed && "text-teal-300")}>
          {f.key}
          {f.careful && <span className="ml-2 text-amber-300 text-xs">asks first</span>}
          {f.at_start && <span className="ml-2 text-muted-foreground text-xs">at the next start</span>}
        </div>
        {f.help && <div className="text-muted-foreground text-xs">{f.help}</div>}
        {f.file_only && <div className="text-muted-foreground text-xs italic">{f.file_only}</div>}
      </div>
      <div className="flex items-center gap-1">
        {editor}
        {!f.file_only && changed && (
          <Button size="icon" variant="ghost" className="size-8" onClick={() => setEdit(undefined)} aria-label={`Undo ${f.path}`} title="Undo">
            <Undo2 />
          </Button>
        )}
        {!f.file_only && f.set && !changed && (
          <Button size="icon" variant="ghost" className="size-8" onClick={() => setEdit(null)} aria-label={`Reset ${f.path}`} title={f.secret ? "Clear it" : "Back to lyra's default (taken out of the file)"}>
            {f.secret ? <X /> : <RotateCcw />}
          </Button>
        )}
      </div>
    </div>
  );
}

export function EverythingTab({ said }: { said: (n: { ok: boolean; text: string }) => void }) {
  const { call } = useLyra();
  const [data, setData] = useState<AllData | null>(null);
  const [edits, setEdits] = useState<Record<string, Json>>({});
  const [q, setQ] = useState("");
  const [open, setOpen] = useState<string[]>([""]);
  const [asking, setAsking] = useState<AnyField[] | null>(null);
  const [busy, setBusy] = useState(false);
  const load = useCallback(() => void call<AllData>("settings_all").then(setData), [call]);
  useEffect(() => load(), [load]);
  const all = useMemo(() => (data?.sections ?? []).flatMap((s) => s.fields), [data]);
  const words = q.trim().toLowerCase();
  const matches = (f: AnyField) => !words || f.path.toLowerCase().includes(words) || f.help.toLowerCase().includes(words);
  const count = Object.keys(edits).length;
  const save = async (confirmed: string[]) => {
    setBusy(true);
    const r = await call<{ ok?: boolean; changed?: string[]; error?: string }>("settings_set", { changes: edits, confirmed });
    setBusy(false);
    setAsking(null);
    if (r?.ok) {
      setEdits({});
      load();
      said({ ok: true, text: r.changed?.length ? `Saved (the file as it was is in History) and applied: ${r.changed.join(", ")}.` : "Nothing changed." });
    } else said({ ok: false, text: r?.error ?? "lyra didn't answer" });
  };
  const trySave = () => {
    const careful = all.filter((f) => f.careful && edits[f.path] !== undefined);
    if (careful.length) setAsking(careful);
    else void save([]);
  };
  if (data?.error) return <Failed error={data.error} />;
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2">
        <Input value={q} onChange={(e) => setQ(e.target.value)} placeholder="Find a setting…" className="h-8 w-64" aria-label="Find a setting" />
        <span className="text-muted-foreground text-xs">{all.length} settings · {data?.path}</span>
        <div className="ml-auto flex items-center gap-2">
          {count > 0 && (
            <Button size="sm" variant="ghost" onClick={() => setEdits({})}>
              Undo all
            </Button>
          )}
          <Button size="sm" disabled={!count || busy} onClick={trySave}>
            <Check /> Save {count ? `${count} change${count === 1 ? "" : "s"}` : ""}
          </Button>
        </div>
      </div>
      {(data?.sections ?? []).map((s) => {
        const fields = s.fields.filter(matches);
        if (words && !fields.length && !s.name.toLowerCase().includes(words)) return null;
        const isOpen = !!words || open.includes(s.name);
        const edited = s.fields.filter((f) => edits[f.path] !== undefined).length;
        return (
          <Card key={s.name} className="gap-0 py-2">
            <button type="button" className="flex w-full items-start gap-2 px-4 py-1.5 text-left" onClick={() => setOpen(isOpen ? open.filter((x) => x !== s.name) : [...open, s.name])} aria-expanded={isOpen}>
              {isOpen ? <ChevronDown className="mt-0.5 size-4 shrink-0" /> : <ChevronRight className="mt-0.5 size-4 shrink-0" />}
              <span className="min-w-0">
                <span className="font-medium font-mono text-sm">{s.title}</span>
                {!s.set && <Badge variant="outline" className="ml-2 text-xs">not in the file</Badge>}
                {edited > 0 && <Badge variant="secondary" className="ml-2 text-xs">{edited} changed</Badge>}
                {s.help && <span className="block text-muted-foreground text-xs">{s.help}</span>}
              </span>
            </button>
            {isOpen && (
              <CardContent className="divide-y divide-border px-4">
                {s.list ? (
                  <div className="flex items-center gap-1.5 py-2 text-muted-foreground text-xs">
                    <Lock className="size-3.5" /> {s.entries?.length ? `${s.entries.join(", ")} · ` : "none · "}
                    {s.file_only}
                  </div>
                ) : (
                  fields.map((f) => (
                    <Row
                      key={f.path}
                      f={f}
                      edit={edits[f.path]}
                      setEdit={(v) =>
                        setEdits((e) => {
                          const next = { ...e };
                          if (v === undefined) delete next[f.path];
                          else next[f.path] = v;
                          return next;
                        })
                      }
                    />
                  ))
                )}
              </CardContent>
            )}
          </Card>
        );
      })}
      {(data?.unknown.length ?? 0) > 0 && (
        <p className="text-muted-foreground text-xs">
          In config.toml, but lyra doesn't read them (left as they are): <span className="font-mono">{data?.unknown.join(", ")}</span>
        </p>
      )}
      <Dialog open={!!asking} onOpenChange={(o) => !o && setAsking(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Change these?</DialogTitle>
            <DialogDescription>They can lock you out or move lyra's data. The file as it is now is kept in History either way.</DialogDescription>
          </DialogHeader>
          <ul className="space-y-2 text-sm">
            {asking?.map((f) => (
              <li key={f.path}>
                <span className="font-mono">{f.path}</span>
                <div className="text-amber-300 text-xs">{f.careful}</div>
              </li>
            ))}
          </ul>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setAsking(null)}>
              Cancel
            </Button>
            <Button disabled={busy} onClick={() => void save(asking?.map((f) => f.path) ?? [])}>
              Change them
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}

interface Version {
  id: string;
  at: string;
  by: string;
  how: string;
  keys: string[];
}

type DiffLine = { op: "+" | "-" | " " | "gap"; line?: number; text: string };

export function HistoryTab({ said }: { said: (n: { ok: boolean; text: string }) => void }) {
  const { call } = useLyra();
  const [versions, setVersions] = useState<Version[] | null>(null);
  const [error, setError] = useState("");
  const [picked, setPicked] = useState<Version | null>(null);
  const [againstNow, setAgainstNow] = useState(false);
  const [diff, setDiff] = useState<DiffLine[] | null>(null);
  const [restoring, setRestoring] = useState(false);
  const load = useCallback(() => {
    void call<{ versions?: Version[]; error?: string }>("config_history").then((r) => (r.error ? setError(r.error) : setVersions(r.versions ?? [])));
  }, [call]);
  useEffect(() => load(), [load]);
  useEffect(() => {
    if (!picked) return setDiff(null);
    void call<{ lines?: DiffLine[]; error?: string }>("config_diff", { id: picked.id, against_now: againstNow }).then((r) => (r.error ? setError(r.error) : setDiff(r.lines ?? [])));
  }, [picked, againstNow, call]);
  const restore = async () => {
    if (!picked) return;
    const r = await call<{ ok?: boolean; text?: string; error?: string }>("config_restore", { id: picked.id });
    setRestoring(false);
    said({ ok: !!r.ok, text: r.text ?? r.error ?? "lyra didn't answer" });
    if (r.ok) {
      setPicked(null);
      load();
    }
  };
  return (
    <div className="space-y-3">
      <Failed error={error || undefined} />
      <Card className="gap-2 py-3">
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <HistoryIcon className="size-4" /> config.toml's versions
          </CardTitle>
          <CardDescription>Before every change lyra makes (this page, /settings, a reload after an edit by hand), the file as it was is kept. Pick one to see what changed, and put it back if you need to. Keys show as ••••••.</CardDescription>
        </CardHeader>
        <CardContent className="divide-y divide-border px-4 text-sm">
          {versions?.map((v) => (
            <button key={v.id} type="button" onClick={() => setPicked(picked?.id === v.id ? null : v)} className={cn("flex w-full flex-col gap-0.5 py-2 text-left", picked?.id === v.id && "text-teal-200")}>
              <span>
                {new Date(v.at).toLocaleString()} · <span className="text-muted-foreground">{v.by} · {v.how}</span> <span className="text-muted-foreground text-xs">({ago(v.at)})</span>
              </span>
              <span className="truncate font-mono text-muted-foreground text-xs">{v.keys.length ? v.keys.slice(0, 8).join(", ") + (v.keys.length > 8 ? ` +${v.keys.length - 8}` : "") : "(comments or layout only)"}</span>
            </button>
          ))}
          {versions && versions.length === 0 && <p className="py-2 text-muted-foreground">No changes yet: the first save keeps the file as it is now.</p>}
        </CardContent>
      </Card>
      {picked && (
        <Card className="gap-2 py-3">
          <CardHeader className="px-4">
            <CardTitle className="text-sm">{againstNow ? "From before this change to the file now" : "What this change did"}</CardTitle>
            <div className="flex flex-wrap gap-2">
              <Button size="sm" variant={againstNow ? "ghost" : "secondary"} onClick={() => setAgainstNow(false)}>
                This change
              </Button>
              <Button size="sm" variant={againstNow ? "secondary" : "ghost"} onClick={() => setAgainstNow(true)}>
                Compared with now
              </Button>
              <Button size="sm" variant="outline" className="ml-auto" onClick={() => setRestoring(true)}>
                <RotateCcw /> Put back as it was before this
              </Button>
            </div>
          </CardHeader>
          <CardContent className="px-4">
            {diff && diff.length === 0 && <p className="text-muted-foreground text-sm">The same: nothing differs.</p>}
            {diff && diff.length > 0 && (
              <pre className="max-h-[60vh] overflow-auto rounded-md border bg-muted/30 p-2 text-xs leading-relaxed">
                {diff.map((l, i) => (
                  <div key={i} className={cn("whitespace-pre-wrap", l.op === "+" && "bg-emerald-950/40 text-emerald-300", l.op === "-" && "bg-red-950/40 text-red-300", l.op === "gap" && "text-muted-foreground")}>
                    {l.op === "gap" ? "  …" : `${l.op} ${l.text}`}
                  </div>
                ))}
              </pre>
            )}
          </CardContent>
        </Card>
      )}
      <Dialog open={restoring} onOpenChange={setRestoring}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>Put config.toml back?</DialogTitle>
            <DialogDescription>It goes back to how it was before {picked ? new Date(picked.at).toLocaleString() : "this change"}, and lyra reloads it. The file as it is now is kept in History, so this can be undone the same way.</DialogDescription>
          </DialogHeader>
          <DialogFooter>
            <Button variant="ghost" onClick={() => setRestoring(false)}>
              Cancel
            </Button>
            <Button onClick={() => void restore()}>Put it back</Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </div>
  );
}
