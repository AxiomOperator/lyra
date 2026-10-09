// Users (admins): who uses lyra, letting new sign-ins in, roles, and turning
// someone off, and each person's own tool-call limit. Changes go through
// lyra's own `/users` command.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { Check, Copy, KeyRound, ShieldCheck, UserMinus, UserPlus, X } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Input } from "@/components/ui/input";
import { Back, Failed, Page, useAction, useConfirm } from "./parts";
import { ago } from "./push";
import { useData, useLyra } from "./store";
import type { UserRow } from "./types";

type Issued = { name: string; username: string; password: string };

/** A one-time password, shown once: copy it and give it to them. */
function IssuedCard({ issued, onClose }: { issued: Issued; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  return (
    <Card className="gap-2 border-teal-700/60 py-3">
      <CardHeader className="px-4">
        <CardTitle className="text-sm">{issued.name} can sign in now</CardTitle>
        <CardDescription>Give them these. The password works once: they choose their own when they sign in. It isn't shown again.</CardDescription>
        <CardAction>
          <Button size="icon" variant="ghost" className="size-7" aria-label="Done" onClick={onClose}>
            <X className="size-4" />
          </Button>
        </CardAction>
      </CardHeader>
      <CardContent className="flex flex-wrap items-center gap-3 px-4 font-mono text-sm">
        <span>
          <span className="text-muted-foreground">username</span> {issued.username}
        </span>
        <span>
          <span className="text-muted-foreground">password</span> {issued.password}
        </span>
        <Button
          size="sm"
          variant="secondary"
          onClick={() =>
            void navigator.clipboard?.writeText(`${location.origin}\nusername: ${issued.username}\npassword: ${issued.password}`).then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 1500);
            })
          }
        >
          {copied ? <Check /> : <Copy />} {copied ? "Copied" : "Copy"}
        </Button>
      </CardContent>
    </Card>
  );
}

/** A new account without Microsoft: a name, a username, admin or not. */
function AddPerson({ onIssued }: { onIssued: (i: Issued) => void }) {
  const { call } = useLyra();
  const [name, setName] = useState("");
  const [username, setUsername] = useState("");
  const [admin, setAdmin] = useState(false);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const add = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    const r = await call<Issued & { error?: string }>("user_add", { name, username, admin });
    setBusy(false);
    if (r?.error) return setError(r.error);
    setError("");
    setName("");
    setUsername("");
    setAdmin(false);
    onIssued(r);
  };
  return (
    <Card className="gap-2 py-3">
      <CardHeader className="px-4">
        <CardTitle className="text-sm">Add a person</CardTitle>
        <CardDescription>An account with a username and password: no Microsoft 365 needed. They can connect their own Outlook later if they have one.</CardDescription>
      </CardHeader>
      <CardContent className="px-4">
        <form onSubmit={(e) => void add(e)} className="flex flex-wrap items-center gap-2">
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Their name" className="min-w-40 flex-1" required aria-label="Their name" />
          <Input
            value={username}
            onChange={(e) => setUsername(e.target.value.toLowerCase())}
            placeholder="username"
            autoCapitalize="none"
            spellCheck={false}
            className="min-w-32 flex-1 font-mono"
            required
            aria-label="Username"
          />
          <label className="flex items-center gap-1.5 text-sm">
            <input type="checkbox" checked={admin} onChange={(e) => setAdmin(e.target.checked)} /> admin
          </label>
          <Button type="submit" disabled={busy}>
            <UserPlus /> Add
          </Button>
        </form>
        {error && <p className="mt-2 text-red-300 text-sm">{error}</p>}
      </CardContent>
    </Card>
  );
}

/** Someone's limit on tool calls in one reply: the default, or their own. */
function Rounds({ u, busy, set }: { u: UserRow; busy: boolean; set: (v: string) => void }) {
  const steps = [8, 16, 24, 32, 48, 64];
  const own = u.tool_rounds ?? null;
  const options = own && !steps.includes(own) ? [...steps, own].sort((a, b) => a - b) : steps;
  return (
    <label className="flex items-center gap-1 text-muted-foreground text-xs" title="Rounds of tool calls in one reply before lyra stops and offers Continue">
      Tool calls
      <select
        className="h-8 rounded-md border bg-transparent px-2 text-foreground text-xs"
        disabled={busy}
        value={own === null ? "default" : String(own)}
        onChange={(e) => set(e.target.value)}
      >
        <option value="default">default ({u.default_rounds ?? 8})</option>
        {options.map((n) => (
          <option key={n} value={String(n)}>
            {n}
          </option>
        ))}
      </select>
    </label>
  );
}

export function UsersPage({ onBack }: { onBack: () => void }) {
  const { status, user: me, call } = useLyra();
  const [issued, setIssued] = useState<Issued | null>(null);
  const [failed, setFailed] = useState("");
  // A new one-time password (forgotten, or a username for someone who has none).
  const reset = async (u: UserRow) => {
    const username = u.username ? undefined : prompt(`A username for ${u.name} to sign in with (letters, digits, dots):`)?.trim();
    if (!u.username && !username) return;
    const r = await call<Issued & { error?: string }>("user_password", { id: u.id, username });
    if (r?.error) return setFailed(r.error);
    setFailed("");
    setIssued(r);
    reload();
  };
  // Asked again when someone new signs in.
  const [data, reload] = useData<UserRow[] | { error: string }>("users", [status.users_waiting]);
  const { act, busy, note } = useAction(reload);
  const [confirm, dialog] = useConfirm();
  const users = Array.isArray(data) ? data : [];
  const key = (u: UserRow) => u.email || u.username || u.name;
  const waiting = users.filter((u) => u.status === "pending");
  const rest = users.filter((u) => u.status !== "pending");
  return (
    <Page title="Users" description="The people who use lyra: accounts you add here sign in with a username and password; Microsoft sign-ins wait here until you let them in." action={<Back onBack={onBack} />}>
      {data && !Array.isArray(data) && <Failed error={data.error} />}
      {note}
      <Failed error={failed || undefined} />
      {issued && <IssuedCard issued={issued} onClose={() => setIssued(null)} />}
      <AddPerson
        onIssued={(i) => {
          setIssued(i);
          reload();
        }}
      />
      {waiting.length > 0 && (
        <Card className="gap-1 border-amber-700/50 py-3">
          <CardHeader className="px-4">
            <CardTitle className="text-sm">Waiting to be let in</CardTitle>
            <CardDescription>They signed in with Microsoft. They'll be members; make someone an admin afterwards if needed.</CardDescription>
          </CardHeader>
          <CardContent className="divide-y px-4">
            {waiting.map((u) => (
              <div key={u.id} className="flex items-center gap-2 py-2">
                <div className="min-w-0 flex-1">
                  <div className="text-sm">{u.name}</div>
                  <div className="text-muted-foreground text-xs">
                    {u.email} · signed in {ago(u.last_seen ?? u.created)}
                  </div>
                </div>
                <Button size="sm" disabled={busy} onClick={() => void act(`/users approve ${key(u)}`)}>
                  <Check /> Let in
                </Button>
                <Button size="sm" variant="ghost" disabled={busy} onClick={() => void act(`/users decline ${key(u)}`)}>
                  <X /> Decline
                </Button>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
      <Card className="gap-0 py-3">
        <CardHeader className="px-4">
          <CardTitle className="text-sm">Everyone</CardTitle>
          <CardAction className="text-muted-foreground text-xs">{rest.length}</CardAction>
        </CardHeader>
        <CardContent className="divide-y px-4">
          {rest.map((u) => {
            const self = me?.user === u.id;
            return (
              <div key={u.id} className={cn("flex flex-wrap items-center gap-2 py-2", u.status === "disabled" && "opacity-60")}>
                <div className="min-w-0 flex-1">
                  <div className="flex items-center gap-2 text-sm">
                    {u.name}
                    {self && <span className="text-muted-foreground text-xs">(you)</span>}
                    <Badge variant="outline" className={u.role === "admin" ? "border-teal-700/60 text-teal-300" : ""}>
                      {u.role}
                    </Badge>
                    {u.status === "disabled" && <Badge variant="outline">off</Badge>}
                  </div>
                  <div className="text-muted-foreground text-xs">
                    {[
                      u.username && `signs in as ${u.username}${u.must_change ? " (hasn't chosen a password yet)" : ""}`,
                      u.email,
                      !u.username && !u.microsoft && !u.devices.length && "no way to sign in yet: set a password",
                      u.devices.length > 0 && `${u.devices.length} device${u.devices.length === 1 ? "" : "s"}`,
                      u.last_seen && `seen ${ago(u.last_seen)}`,
                    ]
                      .filter(Boolean)
                      .join(" · ")}
                  </div>
                </div>
                {u.status === "active" && <Rounds u={u} busy={busy} set={(v) => void act(`/users rounds ${key(u)} ${v}`)} />}
                {u.status === "active" && (
                  <Button size="sm" variant="ghost" disabled={busy} onClick={() => void reset(u)} title={u.username ? "A new one-time password" : "A username and password, for signing in without Microsoft"}>
                    <KeyRound /> {u.username ? "Reset password" : "Set password"}
                  </Button>
                )}
                {!self && u.status === "active" && (
                  <Button size="sm" variant="ghost" disabled={busy} onClick={() => void act(`/users ${u.role === "admin" ? "member" : "admin"} ${key(u)}`)}>
                    <ShieldCheck /> {u.role === "admin" ? "Make member" : "Make admin"}
                  </Button>
                )}
                {!self && u.status === "active" && (
                  <Button
                    size="sm"
                    variant="ghost"
                    disabled={busy}
                    onClick={() => confirm({ title: `Turn off ${u.name}?`, text: "Their devices stop working at once. You can turn them on again.", action: "Turn off", run: () => void act(`/users disable ${key(u)}`) })}
                  >
                    <UserMinus /> Turn off
                  </Button>
                )}
                {u.status === "disabled" && (
                  <Button size="sm" variant="ghost" disabled={busy} onClick={() => void act(`/users enable ${key(u)}`)}>
                    <UserPlus /> Turn on
                  </Button>
                )}
              </div>
            );
          })}
        </CardContent>
      </Card>
      {dialog}
    </Page>
  );
}
