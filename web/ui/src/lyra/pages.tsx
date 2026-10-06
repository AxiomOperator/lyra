// The other tabs: machines, devices, activity, and more (conversations,
// notifications, lyra's agents/goals/skills/memory, about, updates).

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { Bell, BellOff, Check, Copy, MessageSquarePlus, RefreshCw, Server, Smartphone, Terminal, Trash2, Unplug, Download } from "lucide-react";
import { useEffect, useState, type ReactNode } from "react";
import { PairCard } from "./chat";
import { ago, blocker, disable, enable, test } from "./push";
import { APP_VERSION, useData, useLyra } from "./store";
import type { About, ActivityLine, Device, Session } from "./types";

function Page({ title, description, action, children }: { title: string; description?: string; action?: ReactNode; children: ReactNode }) {
  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-4 pt-4 pb-8">
      <div className="mx-auto max-w-2xl space-y-4">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h1 className="font-semibold text-xl">{title}</h1>
            {description && <p className="text-muted-foreground text-sm">{description}</p>}
          </div>
          {action}
        </div>
        {children}
      </div>
    </div>
  );
}

function Dot({ on }: { on: boolean }) {
  return <span className={cn("inline-block size-2.5 shrink-0 rounded-full", on ? "bg-emerald-500 shadow-[0_0_8px] shadow-emerald-500/60" : "bg-muted-foreground/40")} />;
}

/** Ask before something that can't be undone. */
function useConfirm() {
  const [ask, setAsk] = useState<{ title: string; text: string; action: string; run: () => void } | null>(null);
  const dialog = (
    <Dialog open={!!ask} onOpenChange={(o) => !o && setAsk(null)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{ask?.title}</DialogTitle>
          <DialogDescription>{ask?.text}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={() => setAsk(null)}>Cancel</Button>
          <Button
            variant="destructive"
            onClick={() => {
              ask?.run();
              setAsk(null);
            }}
          >
            {ask?.action}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
  return [setAsk, dialog] as const;
}

// ---- machines

export function MachinesPage({ mention }: { mention: (name: string) => void }) {
  const { status, say } = useLyra();
  const [confirm, dialog] = useConfirm();
  const [copied, setCopied] = useState(false);
  const machines = status.machines_detail ?? [];
  const install = `curl -fsSL ${location.origin}/install.sh | sh -s -- --name NAME`;
  return (
    <Page title="Machines" description="Where lyra's Operator can work. Mention one in chat with @name.">
      {(status.pairing ?? []).map((p) => (
        <PairCard key={p.id} p={p} />
      ))}
      <Card className="py-4">
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <Dot on /> server
          </CardTitle>
          <CardDescription>Where lyra itself runs.</CardDescription>
          <CardAction>
            <Badge variant="secondary">home</Badge>
          </CardAction>
        </CardHeader>
      </Card>
      {machines.map((m) => (
        <Card key={m.id} className="py-4">
          <CardHeader className="px-4">
            <CardTitle className="flex items-center gap-2 text-base">
              <Dot on={m.online} /> {m.name}
            </CardTitle>
            <CardDescription className="break-words">
              {m.online
                ? [m.hostname, m.os, m.user && `as ${m.user}`, m.version && (m.self_update ? `lyra-node ${m.version} · ${m.build}` : `node built into lyra ${m.version}`)].filter(Boolean).join(" · ")
                : `offline · last seen ${ago(m.last_seen)}`}
            </CardDescription>
            <CardAction className="flex gap-1.5">
              {m.update_available && <Badge className="bg-amber-500/20 text-amber-300">update</Badge>}
              <Badge variant={m.online ? "default" : "secondary"} className={m.online ? "bg-emerald-600/80 text-white" : ""}>
                {m.online ? "online" : "offline"}
              </Badge>
            </CardAction>
          </CardHeader>
          <CardContent className="flex flex-wrap gap-2 px-4">
            {m.online && (
              <Button size="sm" variant="secondary" onClick={() => mention(m.name)}>
                <Terminal /> Ask on @{m.name}
              </Button>
            )}
            {m.online && m.self_update && (
              <Button size="sm" variant="secondary" onClick={() => say(`/machines update ${m.name}`)}>
                <Download /> {m.update_available ? "Update" : "Reinstall latest"}
              </Button>
            )}
            <Button
              size="sm"
              variant="ghost"
              className="text-red-400 hover:text-red-300"
              onClick={() =>
                confirm({
                  title: `Remove ${m.name}?`,
                  text: m.online ? "lyra-node uninstalls itself from that machine (service, settings and program) and is unpaired." : "It's offline, so it will only be unpaired; its files stay on that machine.",
                  action: "Remove",
                  run: () => say(`/machines remove ${m.name}`),
                })
              }
            >
              <Trash2 /> Remove
            </Button>
          </CardContent>
        </Card>
      ))}
      <Card className="py-4">
        <CardHeader className="px-4">
          <CardTitle className="text-base">Add a machine</CardTitle>
          <CardDescription>On the machine (x86_64 Linux), run this — then approve its pairing request here.</CardDescription>
        </CardHeader>
        <CardContent className="flex gap-2 px-4">
          <code className="min-w-0 flex-1 overflow-x-auto whitespace-nowrap rounded-md bg-black/40 px-3 py-2 font-mono text-cyan-300 text-sm">{install}</code>
          <Button
            size="icon"
            variant="secondary"
            onClick={async () => {
              try {
                await navigator.clipboard.writeText(install);
                setCopied(true);
                setTimeout(() => setCopied(false), 2000);
              } catch {
                // no clipboard: the text is selectable
              }
            }}
          >
            {copied ? <Check /> : <Copy />}
          </Button>
        </CardContent>
      </Card>
      {dialog}
    </Page>
  );
}

// ---- devices

export function DevicesPage() {
  const { status, say, device } = useLyra();
  const [confirm, dialog] = useConfirm();
  // Follows who's online (not every status tick).
  const who = JSON.stringify([status.online ?? [], (status.machines_detail ?? []).map((m) => [m.name, m.online])]);
  const [devices, refresh] = useData<Device[]>("devices", [who]);
  return (
    <Page title="Devices" description="Phones, browsers, terminals and machines paired with lyra.">
      {(status.pairing ?? []).map((p) => (
        <PairCard key={p.id} p={p} />
      ))}
      {(devices ?? []).map((d) => {
        const mine = device?.id === d.id;
        const Icon = d.kind === "node" ? Server : d.name.toLowerCase().includes("terminal") ? Terminal : Smartphone;
        return (
          <Card key={d.id} className={cn("py-4", mine && "border-teal-500/60")}>
            <CardHeader className="px-4">
              <CardTitle className="flex items-center gap-2 text-base">
                <Dot on={d.online} />
                <Icon className="size-4 text-muted-foreground" /> {d.name}
              </CardTitle>
              <CardDescription>
                paired {ago(d.created)} · last seen {ago(d.last_seen)}
                {d.push && " · notifications on"}
                {mine && " · this device"}
              </CardDescription>
              <CardAction className="flex gap-1.5">
                <Badge variant="secondary">{d.kind === "node" ? "machine" : "device"}</Badge>
              </CardAction>
            </CardHeader>
            {!mine && (
              <CardContent className="px-4">
                <Button
                  size="sm"
                  variant="ghost"
                  className="text-red-400 hover:text-red-300"
                  onClick={() =>
                    confirm({
                      title: `Unpair ${d.name}?`,
                      text: "It will need to pair again to use lyra.",
                      action: "Unpair",
                      run: () => {
                        say(`/devices remove ${d.id}`);
                        setTimeout(refresh, 800);
                      },
                    })
                  }
                >
                  <Unplug /> Unpair
                </Button>
              </CardContent>
            )}
          </Card>
        );
      })}
      {dialog}
    </Page>
  );
}

// ---- activity

const levelColor: Record<string, string> = {
  error: "text-red-400",
  agent: "text-sky-400",
  tool: "text-amber-300",
  learn: "text-fuchsia-400",
  memory: "text-cyan-300",
  plan: "text-blue-300",
  evolve: "text-emerald-400",
};

export function ActivityPage() {
  const [lines, refresh] = useData<ActivityLine[]>("activity");
  useEffect(() => {
    const t = setInterval(() => document.visibilityState === "visible" && refresh(), 5000);
    return () => clearInterval(t);
  }, [refresh]);
  return (
    <Page
      title="Activity"
      description="What lyra has been doing, newest first."
      action={
        <Button size="icon" variant="ghost" onClick={refresh}>
          <RefreshCw />
        </Button>
      }
    >
      <Card className="py-2">
        <CardContent className="divide-y divide-border px-3">
          {(lines ?? []).map((a, i) => (
            <div key={i} className={cn("py-1.5 text-xs leading-relaxed", levelColor[a.level])}>
              <span className="mr-2 font-mono text-muted-foreground">{a.time}</span>
              <span className="break-words">{a.text}</span>
            </div>
          ))}
        </CardContent>
      </Card>
    </Page>
  );
}

// ---- more

function Notifications() {
  const { token, device, setPush } = useLyra();
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);
  const why = blocker();
  const on = !!device?.push && typeof Notification !== "undefined" && Notification.permission === "granted";
  const toggle = async () => {
    setBusy(true);
    setNote("");
    try {
      if (on) {
        await disable(token);
        setPush(false);
      } else {
        await enable(token);
        setPush(true);
      }
    } catch (e) {
      setNote(`Couldn't turn notifications on: ${(e as Error).message}`);
    }
    setBusy(false);
  };
  return (
    <Card className="py-4">
      <CardHeader className="px-4">
        <CardTitle className="text-base">Notifications</CardTitle>
        <CardDescription>{device ? `On ${device.name}: replies, approvals and pairing requests while lyra isn't open.` : ""}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-2 px-4">
        <div className="flex flex-wrap gap-2">
          <Button disabled={!!why || busy} onClick={toggle} variant={on ? "secondary" : "default"}>
            {on ? <BellOff /> : <Bell />} {on ? "Turn off" : "Turn on"}
          </Button>
          {on && (
            <Button variant="secondary" onClick={async () => setNote(await test(token))}>
              Send a test
            </Button>
          )}
        </div>
        {(why || note) && <p className="text-muted-foreground text-sm">{why || note}</p>}
      </CardContent>
    </Card>
  );
}

export function MorePage({ toChat, update }: { toChat: () => void; update: () => void }) {
  const { say, unpaired, token, serverVersion } = useLyra();
  const [sessions] = useData<Session[]>("sessions");
  const [about] = useData<About>("about");
  const { ask, onData } = useLyra();
  const [text, setText] = useState<{ title: string; body: string } | null>(null);
  const [confirm, dialog] = useConfirm();
  useEffect(() => onData((w, d) => ["agents", "goals", "skills", "memory"].includes(w) && setText({ title: w[0].toUpperCase() + w.slice(1), body: (d as { text: string }).text })), [onData]);
  const newer = serverVersion && serverVersion !== APP_VERSION;
  return (
    <Page title="More">
      <Card className="py-4">
        <CardHeader className="px-4">
          <CardTitle className="text-base">Conversations</CardTitle>
          <CardAction>
            <Button
              size="sm"
              onClick={() => {
                say("/new");
                toChat();
              }}
            >
              <MessageSquarePlus /> New
            </Button>
          </CardAction>
        </CardHeader>
        <CardContent className="space-y-1.5 px-4">
          {(sessions ?? []).length === 0 && <p className="text-muted-foreground text-sm">No saved conversations yet.</p>}
          {(sessions ?? []).map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => {
                if (!s.current) say(`/resume ${s.id}`);
                toChat();
              }}
              className={cn("flex w-full items-center justify-between gap-3 rounded-lg border px-3 py-2 text-left hover:bg-accent/50", s.current && "border-teal-500/60")}
            >
              <span className="min-w-0">
                <span className="block truncate text-sm">{s.title || "(untitled)"}</span>
                <span className="block text-muted-foreground text-xs">
                  {s.turns} turns · {ago(s.updated)}
                </span>
              </span>
              {s.current && <Badge className="bg-teal-600/80 text-white">open</Badge>}
            </button>
          ))}
        </CardContent>
      </Card>

      <Notifications />

      <Card className="py-4">
        <CardHeader className="px-4">
          <CardTitle className="text-base">Lyra</CardTitle>
          <CardDescription>What lyra knows and can do.</CardDescription>
        </CardHeader>
        <CardContent className="grid grid-cols-2 gap-2 px-4 sm:grid-cols-4">
          {["agents", "goals", "skills", "memory"].map((w) => (
            <Button key={w} variant="secondary" onClick={() => ask(w)} className="capitalize">
              {w}
            </Button>
          ))}
        </CardContent>
      </Card>

      <Card className="py-4">
        <CardHeader className="px-4">
          <CardTitle className="text-base">About</CardTitle>
          <CardDescription className="break-words">
            lyra {about?.lyra} · app {APP_VERSION}
            {newer ? ` (lyra has ${serverVersion})` : ""} · model {about?.model}
            <br />
            lyra-node on offer: {about?.node_build ?? "none"} · {about?.devices ?? 0} paired devices
          </CardDescription>
        </CardHeader>
        <CardContent className="flex flex-wrap gap-2 px-4">
          <Button variant="secondary" onClick={update}>
            <RefreshCw /> {newer ? "Update the app" : "Check for app update"}
          </Button>
          <Button
            variant="ghost"
            className="text-red-400 hover:text-red-300"
            onClick={() =>
              confirm({
                title: "Unpair this device?",
                text: "You'll need a new code from `lyra pair` to use lyra here again.",
                action: "Unpair",
                run: async () => {
                  await disable(token).catch(() => {});
                  unpaired();
                },
              })
            }
          >
            <Unplug /> Unpair this device
          </Button>
        </CardContent>
      </Card>

      <Dialog open={!!text} onOpenChange={(o) => !o && setText(null)}>
        <DialogContent className="max-h-[85vh] sm:max-w-2xl">
          <DialogHeader>
            <DialogTitle>{text?.title}</DialogTitle>
          </DialogHeader>
          <pre className="max-h-[65vh] overflow-auto whitespace-pre-wrap break-words rounded-md bg-muted/40 p-3 font-mono text-xs">{text?.body}</pre>
        </DialogContent>
      </Dialog>
      {dialog}
    </Page>
  );
}
