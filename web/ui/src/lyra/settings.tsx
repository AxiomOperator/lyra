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

interface EmailKind {
  id: string;
  label: string;
  key: string;
  help: string;
  api: string;
}
interface EmailService {
  id: string;
  kind: string;
  kind_label: string;
  name: string;
  from: string;
  stream: string;
  api_url: string;
  enabled: boolean;
  key_set: boolean;
}

/** One service's form: new (no `s`) or changing one. The key is typed, saved into secrets.toml, never shown. */
function ServiceForm({ kinds, s, done }: { kinds: EmailKind[]; s?: EmailService; done: (saved: boolean) => void }) {
  const { call } = useLyra();
  const [kind, setKind] = useState(s?.kind ?? kinds[0]?.id ?? "postmark");
  const [name, setName] = useState(s?.name ?? "");
  const [from, setFrom] = useState(s?.from ?? "");
  const [key, setKey] = useState("");
  const [stream, setStream] = useState(s?.stream ?? "");
  const [api, setApi] = useState(s?.api_url ?? "");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const k = kinds.find((x) => x.id === kind);
  const save = async () => {
    setBusy(true);
    const r = await call<{ ok?: boolean; error?: string }>("email_provider_put", { id: s?.id ?? "", kind, name, from, key, stream, api_url: api });
    setBusy(false);
    if (r?.ok) done(true);
    else setError(r?.error ?? "lyra didn't answer");
  };
  return (
    <div className="space-y-2 rounded-md border p-3">
      <div className="grid gap-2 sm:grid-cols-2">
        <label className="space-y-1 text-xs">
          <span className="text-muted-foreground">Service</span>
          <select value={kind} disabled={!!s} onChange={(e) => setKind(e.target.value)} className="block h-9 w-full rounded-md border bg-background px-2 text-sm" aria-label="Email service">
            {kinds.map((x) => (
              <option key={x.id} value={x.id}>
                {x.label}
              </option>
            ))}
          </select>
        </label>
        <label className="space-y-1 text-xs">
          <span className="text-muted-foreground">Name</span>
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder={k ? `${k.label}` : ""} aria-label="Service name" />
        </label>
        <label className="space-y-1 text-xs sm:col-span-2">
          <span className="text-muted-foreground">From</span>
          <Input value={from} onChange={(e) => setFrom(e.target.value)} placeholder="lyra <lyra@example.org>" aria-label="From address" />
        </label>
        <label className="space-y-1 text-xs sm:col-span-2">
          <span className="text-muted-foreground">
            {k?.key ?? "Key"}
            {s?.key_set ? " (set: leave empty to keep it)" : ""}
          </span>
          <Input type="password" value={key} onChange={(e) => setKey(e.target.value)} placeholder={s?.key_set ? "••••••••" : "Paste it"} autoComplete="off" aria-label="Service key" />
        </label>
        {kind === "postmark" && (
          <label className="space-y-1 text-xs">
            <span className="text-muted-foreground">Message stream</span>
            <Input value={stream} onChange={(e) => setStream(e.target.value)} placeholder="outbound" aria-label="Message stream" />
          </label>
        )}
        <label className="space-y-1 text-xs">
          <span className="text-muted-foreground">API address (if not the usual)</span>
          <Input value={api} onChange={(e) => setApi(e.target.value)} placeholder={k?.api} aria-label="API address" />
        </label>
      </div>
      {k && <p className="text-muted-foreground text-xs">{k.help}</p>}
      {error && <p className="text-red-300 text-xs">{error}</p>}
      <div className="flex gap-2">
        <Button size="sm" disabled={busy || !from.trim() || (!s && !key.trim())} onClick={() => void save()}>
          {s ? "Save" : "Add"}
        </Button>
        <Button size="sm" variant="ghost" onClick={() => done(false)}>
          Cancel
        </Button>
      </div>
    </div>
  );
}

/** lyra's mailbox: the email services, tried in this order (the next takes over when one fails). */
function EmailServices() {
  const { call, ready } = useLyra();
  const [data, setData] = useState<{ providers: EmailService[]; kinds: EmailKind[] } | null>(null);
  const [editing, setEditing] = useState<string | null>(null);
  const [said, setSaid] = useState<{ ok: boolean; text: string } | null>(null);
  const [testing, setTesting] = useState("");
  const load = useCallback(() => void call<{ providers: EmailService[]; kinds: EmailKind[] }>("email_providers").then((d) => d && Array.isArray(d.providers) && setData(d)), [call]);
  useEffect(() => {
    if (ready) load();
  }, [ready, load]);
  const act = async (what: string, arg: Record<string, unknown>) => {
    const r = await call<{ ok?: boolean; error?: string }>(what, arg);
    if (!r?.ok) setSaid({ ok: false, text: r?.error ?? "lyra didn't answer" });
    load();
  };
  const test = async (id: string) => {
    setTesting(id);
    const r = await call<{ ok?: boolean; text?: string; error?: string }>("email_provider_test", { id });
    setTesting("");
    setSaid(r?.ok ? { ok: true, text: `${(r.text ?? "sent").replace(/^sent/, "Sent")}. Check your inbox.` } : { ok: false, text: r?.error ?? "lyra didn't answer" });
  };
  if (!data) return null;
  const list = data.providers;
  return (
    <Card className="gap-3 py-4">
      <CardHeader className="px-4">
        <CardTitle className="text-base">Email</CardTitle>
        <CardDescription>
          lyra's own mailbox, for emailing people their routine results, briefing and recap (only ever to themselves). Add one or more services; they're tried in this order, and the next takes over when one fails. Keys go into secrets.toml and are never shown.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-2 px-4">
        {said && <p className={cn("text-xs", said.ok ? "text-teal-300" : "text-red-300")}>{said.text}</p>}
        {!list.length && editing !== "new" && <p className="text-muted-foreground text-sm">No services yet: people with Outlook connected can still send with it (About me).</p>}
        {list.map((s, i) =>
          editing === s.id ? (
            <ServiceForm
              key={s.id}
              kinds={data.kinds}
              s={s}
              done={(saved) => {
                setEditing(null);
                if (saved) load();
              }}
            />
          ) : (
            <div key={s.id} className={cn("flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-sm", !s.enabled && "opacity-60")}>
              <span className="w-5 text-muted-foreground text-xs">{i + 1}</span>
              <div className="min-w-0 flex-1">
                <div className="font-medium">
                  {s.name} <span className="font-normal text-muted-foreground text-xs">· {s.kind_label}</span>
                  {!s.enabled && <span className="text-muted-foreground text-xs"> · off</span>}
                </div>
                <div className="truncate text-muted-foreground text-xs">
                  {s.from} · {s.key_set ? "key set" : <span className="text-amber-300">no key</span>}
                </div>
              </div>
              <Button size="sm" variant="ghost" disabled={!!testing || !s.key_set} onClick={() => void test(s.id)}>
                {testing === s.id ? "Sending…" : "Test"}
              </Button>
              <Button size="sm" variant="ghost" onClick={() => setEditing(s.id)}>
                Edit
              </Button>
              <Button size="sm" variant="ghost" disabled={i === 0} aria-label={`Try ${s.name} sooner`} onClick={() => void act("email_provider_move", { id: s.id, up: true })}>
                ↑
              </Button>
              <Button size="sm" variant="ghost" disabled={i === list.length - 1} aria-label={`Try ${s.name} later`} onClick={() => void act("email_provider_move", { id: s.id, up: false })}>
                ↓
              </Button>
              <Button size="sm" variant="ghost" onClick={() => void act("email_provider_put", { id: s.id, enabled: !s.enabled })}>
                {s.enabled ? "Turn off" : "Turn on"}
              </Button>
              <Button
                size="sm"
                variant="ghost"
                onClick={() => {
                  if (confirm(`Remove ${s.name}? Its key is removed too.`)) void act("email_provider_remove", { id: s.id });
                }}
              >
                Remove
              </Button>
            </div>
          ),
        )}
        {editing === "new" ? (
          <ServiceForm
            kinds={data.kinds}
            done={(saved) => {
              setEditing(null);
              if (saved) load();
            }}
          />
        ) : (
          <Button size="sm" variant="secondary" onClick={() => setEditing("new")}>
            Add an email service
          </Button>
        )}
      </CardContent>
    </Card>
  );
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
      <EmailServices />
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
