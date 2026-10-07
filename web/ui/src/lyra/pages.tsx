// The other tabs: machines, devices, activity, and more (conversations,
// notifications, lyra's agents/goals/skills/memory, about, updates).

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Dialog, DialogContent, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { Activity as ActivityIcon, AlarmClock, Code2, Archive, Bell, BellOff, Bot, Brain, Check, Copy, Cpu, Download, GraduationCap, MessageSquarePlus, RefreshCw, Server, ShieldCheck, Smartphone, Target, Terminal, Trash2, Unplug } from "lucide-react";
import { useEffect, useState } from "react";
import { PairCard } from "./chat";
import { DiagnosisNote } from "./diagnosis";
import { RulesDialog } from "./manage";
import { SearchBox, SearchHits, useConversationSearch } from "./search";
import { Dot, Page, useConfirm } from "./parts";
import { ago, blocker, disable, enable, test } from "./push";
import { APP_VERSION, useData, useLyra } from "./store";
import type { About, ActivityLine, Device, Health, Session } from "./types";

// ---- machine health

/** Disk, memory, load, failed units and updates, red when over a limit. */
function HealthRow({ h, machine, toChat }: { h: Health; machine: string; toChat: () => void }) {
  const disk = [...(h.disks ?? [])].sort((a, b) => b.used_pct - a.used_pct)[0];
  const failed = h.failed_units?.length ?? 0;
  const bad = (word: string) => h.problems.some((p) => p.includes(word));
  const chip = (text: string, warn: boolean, title?: string) => (
    <Badge key={text} variant="outline" title={title} className={cn("font-normal", warn ? "border-red-500/60 bg-red-500/15 text-red-300" : "text-muted-foreground")}>
      {text}
    </Badge>
  );
  return (
    <div className="space-y-1.5">
      <div className="flex flex-wrap gap-1.5">
        {disk && chip(`disk ${disk.used_pct}%`, bad("disk"), (h.disks ?? []).map((d) => `${d.mount}: ${d.used_pct}%`).join("\n"))}
        {h.memory && chip(`mem ${h.memory.used_pct}%`, bad("memory"))}
        {h.load?.length > 0 && chip(`load ${h.load[0].toFixed(1)}`, bad("load"), `${h.load.map((l) => l.toFixed(2)).join(" ")} on ${h.cpus} CPUs`)}
        {failed > 0 && chip(`${failed} failed`, true, h.failed_units?.join("\n"))}
        {h.updates != null && h.updates > 0 && chip(`${h.updates} updates`, false)}
        <span className="self-center text-muted-foreground/70 text-xs">checked {ago(h.at)}</span>
      </div>
      {h.problems.length > 0 && (
        <ul className="space-y-1.5 text-red-300 text-xs">
          {h.problems.map((p) => (
            <li key={p} className="space-y-1">
              <div>⚠ {p}</div>
              <DiagnosisNote machine={machine} problem={p} toChat={toChat} />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

// ---- machines

export function MachinesPage({ mention, toStatus, toChat }: { mention: (name: string) => void; toStatus: () => void; toChat: () => void }) {
  const { status, say } = useLyra();
  const [confirm, dialog] = useConfirm();
  const [copied, setCopied] = useState(false);
  const [rules, setRules] = useState<string | null>(null);
  const machines = status.machines_detail ?? [];
  const [platform, setPlatform] = useState<"linux" | "windows">("linux");
  const install = platform === "linux" ? `curl -fsSL ${location.origin}/install.sh | sh -s -- --name NAME` : `irm ${location.origin}/install.ps1 | iex`;
  return (
    <Page
      title="Machines"
      description="Where lyra's Operator can work. Mention one in chat with @name."
      action={
        <Button size="sm" variant="ghost" onClick={toStatus}>
          <ActivityIcon /> Status
        </Button>
      }
    >
      {(status.pairing ?? []).map((p) => (
        <PairCard key={p.id} p={p} />
      ))}
      <Card className="py-4">
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <Dot on /> server
          </CardTitle>
          <CardDescription>
            Where lyra itself runs.
            {status.server_harnesses && !Object.keys(status.server_harnesses).length && " No coding agents installed here; coding work runs on machines that have them."}
          </CardDescription>
          <CardAction className="flex gap-1.5">
            {Object.keys(status.server_harnesses ?? {}).map((h) => (
              <Badge key={h} variant="outline" title={String(status.server_harnesses?.[h])}>
                {h === "claude" ? "Claude Code" : h === "opencode" ? "OpenCode" : h}
              </Badge>
            ))}
            <Badge variant="secondary">home</Badge>
          </CardAction>
        </CardHeader>
        <CardContent className="space-y-3 px-4">
          {status.server_health && <HealthRow h={status.server_health} machine="server" toChat={toChat} />}
          <Button size="sm" variant="secondary" onClick={() => setRules("server")}>
            <ShieldCheck /> Rules
          </Button>
        </CardContent>
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
              {Object.keys(m.harnesses ?? {}).map((h) => (
                <Badge key={h} variant="outline" title={String(m.harnesses?.[h])}>
                  {h === "claude" ? "Claude Code" : "OpenCode"}
                </Badge>
              ))}
              {m.update_available && <Badge className="bg-amber-500/20 text-amber-300">update</Badge>}
              {m.online && !!m.health?.problems.length && <Badge className="bg-red-500/20 text-red-300">⚠ {m.health.problems.length}</Badge>}
              <Badge variant={m.online ? "default" : "secondary"} className={m.online ? "bg-emerald-600/80 text-white" : ""}>
                {m.online ? "online" : "offline"}
              </Badge>
            </CardAction>
          </CardHeader>
          {m.online && m.health && (
            <CardContent className="px-4">
              <HealthRow h={m.health} machine={m.name} toChat={toChat} />
            </CardContent>
          )}
          <CardContent className="flex flex-wrap gap-2 px-4">
            {m.online && (
              <Button size="sm" variant="secondary" onClick={() => mention(m.name)}>
                <Terminal /> Ask on @{m.name}
              </Button>
            )}
            {m.online && (
              <Button size="sm" variant="secondary" onClick={() => setRules(m.name)}>
                <ShieldCheck /> Rules
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
          <CardDescription>
            {platform === "linux"
              ? "On the machine (x86_64 Linux), run this — then approve its pairing request here."
              : "On the PC, open PowerShell as administrator and run this — it installs the lyra node service; then approve its pairing request here."}
          </CardDescription>
          <CardAction className="flex gap-1">
            {(["linux", "windows"] as const).map((p) => (
              <Button key={p} size="sm" variant={platform === p ? "secondary" : "ghost"} className="h-7 text-xs" onClick={() => setPlatform(p)}>
                {p === "linux" ? "Linux" : "Windows"}
              </Button>
            ))}
          </CardAction>
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
      <RulesDialog machine={rules} onClose={() => setRules(null)} />
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

/** "2.1 MB" */
function sizeText(n: number) {
  return n >= 1048576 ? `${(n / 1048576).toFixed(1)} MB` : n >= 1024 ? `${Math.round(n / 1024)} KB` : `${n} bytes`;
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

export function MorePage({ toChat, open, update }: { toChat: () => void; open: (page: "routines" | "coding" | "memory" | "skills" | "goals" | "model" | "devices") => void; update: () => void }) {
  const { say, unpaired, token, serverVersion, status } = useLyra();
  const [sessions] = useData<Session[]>("sessions");
  // Asked again when a backup starts or finishes.
  const [about] = useData<About>("about", [status.backup?.name, status.backing_up]);
  const { ask, onData } = useLyra();
  const [text, setText] = useState<{ title: string; body: string } | null>(null);
  const [confirm, dialog] = useConfirm();
  const search = useConversationSearch();
  const { run } = useLyra();
  const [backupNote, setBackupNote] = useState("");
  useEffect(() => onData((w, d) => w === "agents" && setText({ title: "Agents", body: (d as { text: string }).text })), [onData]);
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
          <SearchBox query={search.query} setQuery={search.setQuery} className="mb-2" />
          {search.hits && (
            <SearchHits
              hits={search.hits}
              open={(id, current) => {
                if (!current) say(`/resume ${id}`);
                toChat();
              }}
            />
          )}
          {!search.hits && (sessions ?? []).length === 0 && <p className="text-muted-foreground text-sm">No saved conversations yet.</p>}
          {!search.hits && (sessions ?? []).map((s) => (
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
              <span className="flex shrink-0 gap-1.5">
                {(s.answering || status.conversations?.some((c) => c.session === s.id && c.answering)) && <Badge className="bg-sky-600/80 text-white">answering</Badge>}
                {s.current && <Badge className="bg-teal-600/80 text-white">here</Badge>}
              </span>
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
        <CardContent className="grid grid-cols-2 gap-2 px-4 sm:grid-cols-3">
          <Button variant="secondary" className="md:hidden" onClick={() => open("devices")}>
            <Smartphone /> Devices
          </Button>
          <Button variant="secondary" onClick={() => open("routines")}>
            <AlarmClock /> Routines
          </Button>
          <Button variant="secondary" onClick={() => open("coding")}>
            <Code2 /> Coding
          </Button>
          <Button variant="secondary" onClick={() => open("memory")}>
            <Brain /> Memory
          </Button>
          <Button variant="secondary" onClick={() => open("skills")}>
            <GraduationCap /> Skills
          </Button>
          <Button variant="secondary" onClick={() => open("goals")}>
            <Target /> Goals
          </Button>
          <Button variant="secondary" onClick={() => open("model")}>
            <Cpu /> Model
          </Button>
          <Button variant="secondary" onClick={() => ask("agents")}>
            <Bot /> Agents
          </Button>
        </CardContent>
      </Card>

      <Card className="py-4">
        <CardHeader className="px-4">
          <CardTitle className="text-base">About</CardTitle>
          <CardDescription className="break-words">
            lyra {about?.lyra} · app {APP_VERSION}
            {newer ? ` (lyra has ${serverVersion})` : ""} · model {about?.model}
            {status.decide && ` · decisions by ${status.decide.model} (${status.decide.decided} made, ${status.decide.to_chat} left to the chat model, ${status.decide.ms} ms average)`}
            <br />
            lyra-node on offer: {about?.node_build ?? "none"} · {about?.devices ?? 0} paired devices
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-1 px-4 text-muted-foreground text-sm">
          <div>
            <span className="text-foreground">Backups:</span>{" "}
            {status.backing_up
              ? "backing up now…"
              : about?.backups?.last
                ? `last ${ago(about.backups.last.made)} (${sizeText(about.backups.last.size)})`
                : "none yet"}
            {about?.backups && ` · ${about.backups.enabled ? `nightly at ${about.backups.at}` : "nightly off"}, keeping ${about.backups.keep} in ${about.backups.dir}`}
          </div>
          {backupNote && <div className="text-xs">{backupNote}</div>}
        </CardContent>
        <CardContent className="flex flex-wrap gap-2 px-4">
          <Button
            variant="secondary"
            disabled={status.backing_up}
            onClick={async () => {
              const r = await run("/backup now");
              setBackupNote(r.text);
            }}
          >
            <Archive /> Back up now
          </Button>
          {about?.backups?.last && (
            <Button
              variant="secondary"
              onClick={async () => {
                setBackupNote("downloading…");
                try {
                  const r = await fetch("/api/backups/latest", { headers: { Authorization: "Bearer " + token } });
                  if (!r.ok) throw new Error((await r.json().catch(() => ({}))).error || r.statusText);
                  const url = URL.createObjectURL(await r.blob());
                  const a = document.createElement("a");
                  a.href = url;
                  a.download = about.backups?.last?.name ?? "lyra-backup.tar.gz";
                  a.click();
                  setTimeout(() => URL.revokeObjectURL(url), 10000);
                  setBackupNote("Saved. Keep it somewhere other than the server.");
                } catch (e) {
                  setBackupNote(`Couldn't download it: ${(e as Error).message}`);
                }
              }}
            >
              <Download /> Download latest
            </Button>
          )}
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
