// Share: invite someone into this conversation. Each of them has their own
// copy that lyra keeps the same, and each message runs with its sender's
// own rights (lyra's `/share`, `/unshare`).

import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { UserPlus, Users, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useLyra } from "./store";

type Person = { id: string; name: string };

export function ShareButton() {
  const { status, say, call } = useLyra();
  const [open, setOpen] = useState(false);
  const [people, setPeople] = useState<Person[] | null>(null);
  const inIt = status.shared_with ?? [];
  const host = !inIt.length || !!status.shared_host;
  useEffect(() => {
    if (!open || people) return;
    void call<{ people?: Person[] }>("tasks").then((r) => setPeople(r?.people ?? []));
  }, [open, people, call]);
  // Nothing said yet (no title): nothing to share.
  if (!status.title) return null;
  const others = (people ?? []).filter((p) => !inIt.includes(p.name));
  return (
    <>
      <Button variant="ghost" size="sm" className={cn("gap-1.5", inIt.length ? "text-teal-300" : "text-muted-foreground")} onClick={() => setOpen(true)} aria-label="Share this conversation">
        <Users className="size-4" />
        <span className="hidden text-xs md:inline">{inIt.length ? `Shared · ${inIt.length}` : "Share"}</span>
      </Button>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className="sm:max-w-md">
          <DialogHeader>
            <DialogTitle>Share this conversation</DialogTitle>
            <DialogDescription>
              They see it as it goes and can add to it. Each message runs with its sender's own rights and accounts: what you ask uses yours, what they ask uses theirs.
            </DialogDescription>
          </DialogHeader>
          {inIt.length > 0 && (
            <div className="space-y-1">
              <div className="text-muted-foreground text-xs">In it</div>
              {inIt.map((n, i) => (
                <div key={n} className="flex items-center justify-between gap-2 text-sm">
                  <span>{n}</span>
                  {host && i > 0 && (
                    <Button size="sm" variant="ghost" className="text-red-300" onClick={() => (say(`/unshare ${n}`), setOpen(false))}>
                      <X /> Take out
                    </Button>
                  )}
                </div>
              ))}
              {!host && (
                <Button size="sm" variant="ghost" className="text-red-300" onClick={() => (say("/unshare me"), setOpen(false))}>
                  <X /> Leave it
                </Button>
              )}
            </div>
          )}
          {host && (
            <div className="space-y-1">
              <div className="text-muted-foreground text-xs">Invite</div>
              {people === null && <p className="text-muted-foreground text-xs">Looking…</p>}
              {people && others.length === 0 && <p className="text-muted-foreground text-xs">{people.length ? "Everyone's in it." : "Nobody else uses lyra yet."}</p>}
              {others.map((p) => (
                <div key={p.id} className="flex items-center justify-between gap-2 text-sm">
                  <span>{p.name}</span>
                  <Button size="sm" variant="secondary" onClick={() => (say(`/share ${p.name}`), setOpen(false))}>
                    <UserPlus /> Invite
                  </Button>
                </div>
              ))}
            </div>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
