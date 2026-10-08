// More → Settings (admins): the settings people change most — the models,
// working hours, the briefing and recap, notifications — instead of editing
// config.toml. lyra checks every value, writes the file (its comments kept)
// and reloads. Everything else stays in config.toml.

import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Check, RotateCcw, X } from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { Back, Failed, Page } from "./parts";
import { useLyra } from "./store";

type Value = string | number | boolean | string[];
type Field = { key: string; label: string; help: string; kind: "model" | "url" | "time" | "days" | "schedule" | "bool" | "int" | "real" | "text" | "list"; value: Value; min?: number; max?: number };
type Group = { id: string; title: string; help: string; fields: Field[] };
type SettingsData = { groups?: Group[]; path?: string; error?: string };
type Saved = { ok: boolean; changed?: string[]; page?: SettingsData; error?: string };

/** On/off, as a switch. */
function Toggle({ on, set, label }: { on: boolean; set: (v: boolean) => void; label: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={on}
      aria-label={label}
      onClick={() => set(!on)}
      className={cn("relative inline-flex h-6 w-11 shrink-0 items-center rounded-full border transition-colors", on ? "border-teal-600 bg-teal-600" : "border-muted-foreground/40 bg-muted")}
    >
      <span className={cn("inline-block size-4 rounded-full shadow transition-transform", on ? "translate-x-6 bg-white" : "translate-x-1 bg-muted-foreground")} />
    </button>
  );
}

const same = (a: Value, b: Value) => JSON.stringify(a) === JSON.stringify(b);

function Editor({ f, value, set, models }: { f: Field; value: Value; set: (v: Value) => void; models: string[] }) {
  switch (f.kind) {
    case "bool":
      return <Toggle on={value === true} set={set} label={f.label} />;
    case "time":
      return (
        <div className="flex items-center gap-1">
          <Input type="time" className="w-32" value={String(value)} onChange={(e) => set(e.target.value)} aria-label={f.label} />
          {f.key === "recap.at" && value !== "" && (
            <Button size="icon" variant="ghost" onClick={() => set("")} aria-label="End of the working day">
              <X />
            </Button>
          )}
        </div>
      );
    case "int":
    case "real":
      return (
        <Input
          type="number"
          className="w-28"
          min={f.min}
          max={f.max}
          step={f.kind === "real" ? "any" : 1}
          value={String(value)}
          onChange={(e) => set(e.target.value === "" ? "" : Number(e.target.value))}
          aria-label={f.label}
        />
      );
    case "list":
      return <Input className="sm:w-72" value={Array.isArray(value) ? value.join(", ") : String(value)} onChange={(e) => set(e.target.value.split(",").map((s) => s.trimStart()))} placeholder="none" aria-label={f.label} />;
    case "model":
      return (
        <>
          <Input className="sm:w-72" value={String(value)} onChange={(e) => set(e.target.value)} list={f.key === "model" ? "lyra-models" : undefined} placeholder="not set up" aria-label={f.label} />
          {f.key === "model" && (
            <datalist id="lyra-models">
              {models.map((m) => (
                <option key={m} value={m} />
              ))}
            </datalist>
          )}
        </>
      );
    default:
      return <Input className={cn(f.kind === "text" ? "w-24" : "sm:w-72")} value={String(value)} onChange={(e) => set(e.target.value)} placeholder={f.kind === "url" ? "not set up" : undefined} aria-label={f.label} />;
  }
}

export function SettingsPage({ onBack }: { onBack: () => void }) {
  const { call, ready } = useLyra();
  const [data, setData] = useState<SettingsData | null>(null);
  const [edits, setEdits] = useState<Record<string, Value>>({});
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const [models, setModels] = useState<string[]>([]);
  const load = useCallback(() => void call<SettingsData>("settings").then(setData), [call]);
  useEffect(() => {
    if (!ready) return;
    load();
    void call<{ models?: string[] }>("models").then((m) => setModels(m?.models ?? []));
  }, [ready, load, call]);

  const fields = useMemo(() => (data?.groups ?? []).flatMap((g) => g.fields), [data]);
  // Only what differs from what's in the file now goes to lyra.
  const changes = useMemo(() => {
    const out: Record<string, Value> = {};
    for (const f of fields) {
      const v = edits[f.key];
      if (v !== undefined && !same(v, f.value)) out[f.key] = Array.isArray(v) ? v.map((s) => s.trim()).filter(Boolean) : v;
    }
    return out;
  }, [edits, fields]);
  const count = Object.keys(changes).length;

  const save = async () => {
    setBusy(true);
    const r = await call<Saved>("settings_set", { changes });
    setBusy(false);
    if (r?.ok) {
      if (r.page) setData(r.page);
      setEdits({});
      setNote({ ok: true, text: r.changed?.length ? `Saved and applied: ${r.changed.join(", ")}.` : "Nothing changed." });
    } else {
      setNote({ ok: false, text: r?.error ?? "lyra didn't answer" });
    }
  };

  return (
    <Page title="Settings" description="The ones people change most. lyra checks each value, saves config.toml (keeping its comments) and applies it at once." action={<Back onBack={onBack} />}>
      {data?.error && <Failed error={data.error} />}
      {note && (
        <div className={cn("flex items-start justify-between gap-2 rounded-md border px-3 py-2 text-sm", note.ok ? "border-teal-700/60 bg-teal-950/30 text-teal-100" : "border-red-800/60 bg-red-950/30 text-red-200")}>
          <span>{note.text}</span>
          <button type="button" onClick={() => setNote(null)} className="text-muted-foreground hover:text-foreground" aria-label="Dismiss">
            <X className="size-4" />
          </button>
        </div>
      )}
      {(data?.groups ?? []).map((g) => (
        <Card key={g.id} className="gap-3 py-4">
          <CardHeader className="px-4">
            <CardTitle className="text-base">{g.title}</CardTitle>
            <CardDescription>{g.help}</CardDescription>
          </CardHeader>
          <CardContent className="divide-y divide-border px-4">
            {g.fields.map((f) => {
              const value = edits[f.key] ?? f.value;
              const changed = edits[f.key] !== undefined && !same(edits[f.key], f.value);
              return (
                <div key={f.key} className="flex flex-col gap-2 py-3 sm:flex-row sm:items-center sm:justify-between">
                  <div className="min-w-0">
                    <div className={cn("font-medium text-sm", changed && "text-teal-300")}>{f.label}</div>
                    {f.help && <div className="text-muted-foreground text-xs">{f.help}</div>}
                  </div>
                  <Editor f={f} value={value} set={(v) => setEdits((e) => ({ ...e, [f.key]: v }))} models={models} />
                </div>
              );
            })}
          </CardContent>
        </Card>
      ))}
      {data?.path && <p className="text-muted-foreground text-xs">Everything else is in {data.path} (see config.example.toml).</p>}
      {count > 0 && (
        <div className="sticky bottom-0 flex items-center justify-end gap-2 border-t bg-background/95 py-3 backdrop-blur">
          <span className="mr-auto text-muted-foreground text-sm">
            {count} change{count === 1 ? "" : "s"}
          </span>
          <Button variant="ghost" onClick={() => setEdits({})} disabled={busy}>
            <RotateCcw /> Undo
          </Button>
          <Button onClick={() => void save()} disabled={busy}>
            <Check /> {busy ? "Saving…" : "Save"}
          </Button>
        </div>
      )}
    </Page>
  );
}
