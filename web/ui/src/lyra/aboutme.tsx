// "What lyra knows about me": everything lyra keeps about you on one page —
// your memories, how you write, your connected accounts and devices, and what
// lyra did for you, each with its Why? — to correct, delete or export.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Brain, Check, ChevronDown, ChevronRight, Download, HelpCircle, KeyRound, Link2, PenLine, Smartphone, Trash2, X } from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { Back, Failed, Page, useAction, useConfirm } from "./parts";
import { ago } from "./push";
import { useLyra } from "./store";

interface Memory {
  id: string;
  kind: string;
  content: string;
}
interface Why {
  source: string;
  detail: string;
  skills: string[];
}
interface Action {
  at: string;
  tool: string;
  what: string;
  why: Why;
  approved: boolean;
  agent?: string | null;
}
interface Me {
  name?: string | null;
  email?: string | null;
  admin: boolean;
  /** They sign in with a username and password (not only Microsoft). */
  username?: string | null;
  memories: Memory[] | null;
  style: string | null;
  accounts: { microsoft: { connected: boolean; what?: string | null; granted?: string[] | null }; pmi: { connected: boolean } };
  devices: { name: string; last_seen?: string | null }[];
  actions: Action[];
  error?: string;
}

const SOURCE: Record<string, string> = {
  chat: "You asked, in a chat",
  "mail triage": "Mail triage (lyra looks at new mail by itself)",
  "plan my day": "Plan my day (lyra keeps focus time for your tasks)",
  "a plan": "A plan lyra was running",
  lyra: "lyra, by itself",
};

/** One thing lyra did, and its Why?. */
function ActionRow({ a }: { a: Action }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="py-2">
      <button type="button" onClick={() => setOpen(!open)} className="flex w-full items-start gap-2 text-left">
        {open ? <ChevronDown className="mt-0.5 size-4 shrink-0" /> : <ChevronRight className="mt-0.5 size-4 shrink-0" />}
        <span className="min-w-0 flex-1 text-sm">{a.what}</span>
        <span className="shrink-0 text-muted-foreground text-xs">{ago(a.at)}</span>
      </button>
      {open && (
        <div className="mt-1 ml-6 space-y-1 rounded-md border px-3 py-2 text-xs">
          <p className="flex items-center gap-1 font-medium">
            <HelpCircle className="size-3" /> Why?
          </p>
          <p>{SOURCE[a.why.source] ?? a.why.source}</p>
          {a.why.detail && <p className="whitespace-pre-wrap text-muted-foreground">“{a.why.detail}”</p>}
          {!!a.why.skills?.length && <p className="text-muted-foreground">Skills in play: {a.why.skills.join(", ")}</p>}
          {a.agent && <p className="text-muted-foreground">Done by the {a.agent} agent</p>}
          <p className="text-muted-foreground">{a.approved ? "You said yes to it first." : "It didn't need asking (it only touches your own things)."}</p>
          <p className="text-muted-foreground">
            Tool: <code>{a.tool}</code>
          </p>
        </div>
      )}
    </div>
  );
}

function MemoryRow({ m, act, busy }: { m: Memory; act: (c: string) => Promise<boolean>; busy: boolean }) {
  const [edit, setEdit] = useState<string | null>(null);
  return (
    <div className="flex items-start gap-2 py-2">
      <Badge variant="outline" className="mt-0.5 shrink-0">
        {m.kind}
      </Badge>
      {edit === null ? (
        <span className="min-w-0 flex-1 text-sm">{m.content}</span>
      ) : (
        <Input value={edit} onChange={(e) => setEdit(e.target.value)} className="flex-1" aria-label="Corrected memory" autoFocus />
      )}
      {edit === null ? (
        <div className="flex shrink-0 gap-0.5">
          <Button size="icon" variant="ghost" className="size-7" aria-label="Correct it" onClick={() => setEdit(m.content)} disabled={busy}>
            <PenLine className="size-3.5" />
          </Button>
          <Button size="icon" variant="ghost" className="size-7" aria-label="Forget it" onClick={() => void act(`/memory forget ${m.id}`)} disabled={busy}>
            <Trash2 className="size-3.5" />
          </Button>
        </div>
      ) : (
        <div className="flex shrink-0 gap-0.5">
          <Button size="icon" variant="ghost" className="size-7" aria-label="Save" onClick={() => void act(`/memory correct ${m.id} ${edit}`).then((ok) => ok && setEdit(null))} disabled={busy || !edit.trim()}>
            <Check className="size-3.5" />
          </Button>
          <Button size="icon" variant="ghost" className="size-7" aria-label="Cancel" onClick={() => setEdit(null)}>
            <X className="size-3.5" />
          </Button>
        </div>
      )}
    </div>
  );
}

/** Change your own password: the current one, then the new one twice. */
function PasswordCard({ username }: { username: string }) {
  const { token } = useLyra();
  const [old, setOld] = useState("");
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [said, setSaid] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (password !== again) return setSaid({ ok: false, text: "Those two don't match." });
    setBusy(true);
    const r = await fetch("/api/auth/password/change", { method: "POST", headers: { "Content-Type": "application/json", Authorization: "Bearer " + token }, body: JSON.stringify({ old, new: password }) }).catch(() => null);
    const b = r ? await r.json().catch(() => ({})) : { error: "Couldn't reach lyra." };
    setBusy(false);
    if (r?.ok) {
      setOld("");
      setPassword("");
      setAgain("");
      setSaid({ ok: true, text: "Changed. Use the new one next time you sign in." });
    } else setSaid({ ok: false, text: b.error || "That didn't work" });
  };
  return (
    <Card className="gap-1 py-4">
      <CardHeader className="px-4">
        <CardTitle className="flex items-center gap-2 text-base">
          <KeyRound className="size-4" /> Your password
        </CardTitle>
        <CardDescription>
          You sign in as <span className="font-mono">{username}</span>. Forgot it? An admin can give you a new one-time password.
        </CardDescription>
      </CardHeader>
      <CardContent className="px-4">
        <form onSubmit={(e) => void save(e)} className="flex flex-wrap gap-2">
          <Input value={old} onChange={(e) => setOld(e.target.value)} type="password" placeholder="Current password" autoComplete="current-password" className="min-w-40 flex-1" required aria-label="Current password" />
          <Input value={password} onChange={(e) => setPassword(e.target.value)} type="password" placeholder="New password" autoComplete="new-password" className="min-w-40 flex-1" required minLength={10} aria-label="New password" />
          <Input value={again} onChange={(e) => setAgain(e.target.value)} type="password" placeholder="Once more" autoComplete="new-password" className="min-w-40 flex-1" required aria-label="New password again" />
          <Button type="submit" size="sm" variant="secondary" disabled={busy} className="h-9">
            Change it
          </Button>
        </form>
        {said && <p className={cn("mt-2 text-sm", said.ok ? "text-teal-300" : "text-red-300")}>{said.text}</p>}
      </CardContent>
    </Card>
  );
}

export function AboutMePage({ onBack }: { onBack: () => void }) {
  const { call, ready } = useLyra();
  const [me, setMe] = useState<Me | null>(null);
  const load = useCallback(() => void call<Me>("me").then(setMe), [call]);
  const { act, busy, note } = useAction(load);
  const [confirm, dialog] = useConfirm();
  const [all, setAll] = useState(false);
  useEffect(() => {
    if (ready) load();
  }, [ready, load]);

  const exportAll = async () => {
    const data = await call<Me>("me_export");
    const a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([JSON.stringify(data, null, 2)], { type: "application/json" }));
    a.download = `lyra-about-me-${new Date().toISOString().slice(0, 10)}.json`;
    a.click();
    URL.revokeObjectURL(a.href);
  };

  const memories = me?.memories ?? [];
  const shown = all ? memories : memories.slice(0, 15);
  return (
    <Page
      title="What lyra knows about me"
      description="Everything lyra keeps about you, in one place. Correct it, delete it, or take a copy."
      action={
        <div className="flex items-center gap-1">
          <Button size="sm" variant="secondary" onClick={() => void exportAll()}>
            <Download /> Export
          </Button>
          <Back onBack={onBack} />
        </div>
      }
    >
      <Failed error={me?.error} />
      {note}
      <Card className="gap-1 py-4">
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <Brain className="size-4" /> What lyra remembers
          </CardTitle>
          <CardDescription>Things lyra learned from your conversations and keeps for next time. Only you (and lyra, for you) see these.</CardDescription>
          <CardAction className="text-muted-foreground text-xs">{memories.length}</CardAction>
        </CardHeader>
        <CardContent className="divide-y px-4">
          {!memories.length && <p className="py-2 text-muted-foreground text-sm">Nothing yet.</p>}
          {shown.map((m) => (
            <MemoryRow key={m.id} m={m} act={act} busy={busy} />
          ))}
          {memories.length > 15 && (
            <button type="button" className="pt-2 text-muted-foreground text-xs underline" onClick={() => setAll(!all)}>
              {all ? "Show fewer" : `Show all ${memories.length}`}
            </button>
          )}
        </CardContent>
      </Card>
      <Card className="gap-1 py-4">
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <PenLine className="size-4" /> How you write
          </CardTitle>
          <CardDescription>What lyra learned from your sent mail, so its drafts sound like you.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-2 px-4">
          {me?.style ? <pre className="max-h-64 overflow-y-auto whitespace-pre-wrap rounded-md border p-3 text-xs">{me.style}</pre> : <p className="text-muted-foreground text-sm">Not learned yet.</p>}
          <div className="flex gap-2">
            <Button size="sm" variant="secondary" onClick={() => void act("/style learn")} disabled={busy}>
              {me?.style ? "Learn it again" : "Learn it from my mail"}
            </Button>
            {me?.style && (
              <Button size="sm" variant="ghost" onClick={() => confirm({ title: "Forget how you write?", text: "lyra's drafts won't sound like you until it learns again.", action: "Forget", run: () => void act("/style forget") })}>
                <Trash2 /> Forget it
              </Button>
            )}
          </div>
        </CardContent>
      </Card>
      <Card className="gap-1 py-4">
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <Link2 className="size-4" /> Connected accounts
          </CardTitle>
          <CardDescription>What lyra can use on your behalf. Disconnecting stops it at once.</CardDescription>
        </CardHeader>
        <CardContent className="divide-y px-4 text-sm">
          <div className="flex items-center justify-between gap-2 py-2">
            <div>
              <div className="font-medium">Microsoft 365</div>
              <div className="text-muted-foreground text-xs">{me?.accounts.microsoft.connected ? `Your ${me.accounts.microsoft.what}` : "Not connected (More → Outlook)"}</div>
            </div>
            {me?.accounts.microsoft.connected && (
              <Button size="sm" variant="ghost" onClick={() => confirm({ title: "Disconnect Microsoft 365?", text: "lyra stops reading your calendar, mail, Teams and files. Connect again any time.", action: "Disconnect", run: () => void act("/calendar disconnect") })}>
                Disconnect
              </Button>
            )}
          </div>
          <div className="flex items-center justify-between gap-2 py-2">
            <div>
              <div className="font-medium">PMI</div>
              <div className="text-muted-foreground text-xs">{me?.accounts.pmi.connected ? "Your tasks and projects (as you)" : "Not connected (Tasks page)"}</div>
            </div>
            {me?.accounts.pmi.connected && (
              <Button size="sm" variant="ghost" onClick={() => confirm({ title: "Disconnect PMI?", text: "Your access token is removed from lyra. Paste a new one on the Tasks page to connect again.", action: "Disconnect", run: () => void act("/pmi token") })}>
                Disconnect
              </Button>
            )}
          </div>
        </CardContent>
      </Card>
      {me?.username && <PasswordCard username={me.username} />}
      <Card className="gap-1 py-4">
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <Smartphone className="size-4" /> Your devices
          </CardTitle>
          <CardDescription>Phones and browsers signed in as you. An admin can remove one (Devices).</CardDescription>
        </CardHeader>
        <CardContent className="divide-y px-4 text-sm">
          {(me?.devices ?? []).slice(0, 8).map((d, i) => (
            <div key={`${d.name}-${i}`} className="flex justify-between py-1.5">
              <span>{d.name}</span>
              <span className="text-muted-foreground text-xs">{d.last_seen ? `seen ${ago(d.last_seen)}` : ""}</span>
            </div>
          ))}
          {(me?.devices.length ?? 0) > 8 && <p className="py-1.5 text-muted-foreground text-xs">…and {(me?.devices.length ?? 0) - 8} more (the oldest)</p>}
        </CardContent>
      </Card>
      <Card className="gap-1 py-4">
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <HelpCircle className="size-4" /> What lyra did for you
          </CardTitle>
          <CardDescription>Every change lyra made: tap one for its Why? (what prompted it, the skills in play, whether you said yes).</CardDescription>
          {!!me?.actions.length && (
            <CardAction>
              <Button size="sm" variant="ghost" onClick={() => confirm({ title: "Clear this history?", text: "The record goes; what lyra did stays done.", action: "Clear", run: () => void call("me_clear_actions").then(load) })}>
                Clear
              </Button>
            </CardAction>
          )}
        </CardHeader>
        <CardContent className={cn("divide-y px-4")}>
          {!me?.actions.length && <p className="py-2 text-muted-foreground text-sm">Nothing yet.</p>}
          {(me?.actions ?? []).map((a, i) => (
            <ActionRow key={`${a.at}-${i}`} a={a} />
          ))}
        </CardContent>
      </Card>
      {dialog}
    </Page>
  );
}
