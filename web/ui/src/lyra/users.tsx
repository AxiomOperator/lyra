// Users (admins): who uses lyra, letting new sign-ins in, roles, and turning
// someone off. Changes go through lyra's own `/users` command.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { cn } from "@/lib/utils";
import { Check, ShieldCheck, UserMinus, UserPlus, X } from "lucide-react";
import { Back, Failed, useAction } from "./manage";
import { Page, useConfirm } from "./parts";
import { ago } from "./push";
import { useData, useLyra } from "./store";
import type { UserRow } from "./types";

export function UsersPage({ onBack }: { onBack: () => void }) {
  const { status, user: me } = useLyra();
  // Asked again when someone new signs in.
  const [data, reload] = useData<UserRow[] | { error: string }>("users", [status.users_waiting]);
  const { act, busy, note } = useAction(reload);
  const [confirm, dialog] = useConfirm();
  const users = Array.isArray(data) ? data : [];
  const key = (u: UserRow) => u.email || u.name;
  const waiting = users.filter((u) => u.status === "pending");
  const rest = users.filter((u) => u.status !== "pending");
  return (
    <Page title="Users" description="The people who use lyra. New Microsoft sign-ins wait here until you let them in." action={<Back onBack={onBack} />}>
      {data && !Array.isArray(data) && <Failed error={data.error} />}
      {note}
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
                    {u.email || (u.microsoft ? "" : "hasn't signed in with Microsoft yet")}
                    {u.devices.length > 0 && ` · ${u.devices.length} device${u.devices.length === 1 ? "" : "s"}`}
                    {u.last_seen && ` · seen ${ago(u.last_seen)}`}
                  </div>
                </div>
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
