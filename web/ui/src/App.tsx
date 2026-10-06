// lyra's web app: pair once, then chat, machines, devices, activity, more.

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Brain, Cpu, Ellipsis, GraduationCap, MessageSquare, MessageSquarePlus, ScrollText, Server, Smartphone, Sparkles, Target, WifiOff } from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { ChatPage } from "./lyra/chat";
import { GoalsPage, MemoryPage, ModelsPage, SkillsPage } from "./lyra/manage";
import { ActivityPage, DevicesPage, MachinesPage, MorePage } from "./lyra/pages";
import { ago } from "./lyra/push";
import { APP_VERSION, LyraProvider, useData, useLyra } from "./lyra/store";
import type { Session } from "./lyra/types";
import { loadToken, saveToken, takeShared } from "./lyra/token";

type Tab = "chat" | "machines" | "devices" | "activity" | "more" | Manage;

/** Pages reached from More on a phone, and listed in the sidebar on a wide screen. */
type Manage = "memory" | "skills" | "goals" | "model";
const manage: Manage[] = ["memory", "skills", "goals", "model"];

type TabItem = {
  id: Tab;
  label: string;
  icon: typeof MessageSquare;
  badge?: number;
};

/** Wide screens: navigation, conversations and the model on the left. */
function Sidebar({ tabs, more, tab, setTab }: { tabs: TabItem[]; more: TabItem[]; tab: Tab; setTab: (t: Tab) => void }) {
  const { connected, status, say } = useLyra();
  // Refreshed when a conversation gets a title or finishes answering.
  const [sessions] = useData<Session[]>("sessions", [status.title, status.waiting]);
  const open = (s: Session) => {
    if (!s.current) say(`/resume ${s.id}`);
    setTab("chat");
  };
  const item = (t: TabItem) => (
    <button
      key={t.id}
      type="button"
      onClick={() => setTab(t.id)}
      className={cn("flex w-full items-center gap-3 rounded-lg px-3 py-2 text-sm", tab === t.id ? "bg-accent text-teal-300" : "text-muted-foreground hover:bg-accent/50 hover:text-foreground")}
    >
      <t.icon className="size-4" />
      {t.label}
      {!!t.badge && <span className="ml-auto min-w-5 rounded-full bg-amber-400 px-1.5 text-center font-semibold text-[11px] text-black">{t.badge}</span>}
    </button>
  );
  return (
    <aside className="hidden w-64 shrink-0 flex-col border-r bg-card/40 md:flex lg:w-72">
      <div className="flex items-center gap-2.5 px-4 pt-4 pb-3">
        <img src="/icon-192.png" alt="" className="size-8 rounded-lg" />
        <span className="font-semibold text-lg">lyra</span>
        <span title={connected ? "connected" : "not connected"} className={cn("ml-auto size-2.5 rounded-full", connected ? "bg-emerald-500" : "bg-red-500")} />
      </div>
      <nav className="space-y-0.5 px-2">
        {tabs.map(item)}
        <div className="px-3 pt-4 pb-1 font-medium text-muted-foreground text-xs uppercase tracking-wide">Lyra</div>
        {more.map(item)}
      </nav>
      <div className="mt-5 flex items-center justify-between px-4 pb-1.5">
        <span className="font-medium text-muted-foreground text-xs uppercase tracking-wide">Conversations</span>
        <Button
          size="icon"
          variant="ghost"
          className="size-7"
          aria-label="New conversation"
          onClick={() => {
            say("/new");
            setTab("chat");
          }}
        >
          <MessageSquarePlus />
        </Button>
      </div>
      <div className="min-h-0 flex-1 space-y-0.5 overflow-y-auto px-2 pb-2">
        {(sessions ?? []).slice(0, 40).map((s) => (
          <button
            key={s.id}
            type="button"
            onClick={() => open(s)}
            className={cn("flex w-full items-center gap-2 rounded-lg px-3 py-1.5 text-left", s.current && tab === "chat" ? "bg-accent" : "hover:bg-accent/50")}
          >
            <span className="min-w-0 flex-1">
              <span className={cn("block truncate text-sm", s.current ? "text-foreground" : "text-muted-foreground")}>{s.title || "(untitled)"}</span>
              <span className="block text-muted-foreground/70 text-xs">{ago(s.updated)}</span>
            </span>
            {s.answering && <span title="answering" className="size-2 shrink-0 animate-pulse rounded-full bg-sky-400" />}
          </button>
        ))}
      </div>
      <div className="border-t px-4 py-3 text-muted-foreground text-xs">
        <div className="truncate" title={status.model}>
          {status.model}
        </div>
        <div>app {APP_VERSION}</div>
      </div>
    </aside>
  );
}

async function updateApp() {
  try {
    const reg = await navigator.serviceWorker?.getRegistration();
    await reg?.update();
  } catch {
    // reload anyway: the shell is fetched network-first
  }
  location.reload();
}

function Shell() {
  const { status, connected, banner, serverVersion, ready } = useLyra();
  // Opened from Android's share sheet: hand what was shared to the composer.
  useEffect(() => {
    if (!ready || !new URLSearchParams(location.search).has("shared")) return;
    history.replaceState(null, "", "/");
    void takeShared().then((shared) => {
      if (shared) window.dispatchEvent(new CustomEvent("lyra-share", { detail: shared }));
    });
  }, [ready]);
  const [tab, setTab] = useState<Tab>("chat");
  const pairing = status.pairing?.length ?? 0;
  const updates = (status.machines_detail ?? []).filter((m) => m.update_available).length;
  const newer = !!serverVersion && serverVersion !== APP_VERSION;
  const asking = (status.approvals?.length ?? 0) > 0;
  const working = status.phase?.startsWith("↪");

  const mention = (name: string) => {
    setTab("chat");
    // The composer picks it up from here.
    setTimeout(() => window.dispatchEvent(new CustomEvent("lyra-mention", { detail: name })), 0);
  };

  const more: TabItem[] = [
    { id: "memory", label: "Memory", icon: Brain },
    { id: "skills", label: "Skills", icon: GraduationCap },
    { id: "goals", label: "Goals", icon: Target },
    { id: "model", label: "Model", icon: Cpu },
  ];
  const toMore = () => setTab("more");
  const tabs: TabItem[] = [
    { id: "chat", label: "Chat", icon: MessageSquare, badge: asking ? 1 : 0 },
    {
      id: "machines",
      label: "Machines",
      icon: Server,
      badge: pairing + updates,
    },
    { id: "devices", label: "Devices", icon: Smartphone, badge: pairing },
    { id: "activity", label: "Activity", icon: ScrollText },
    { id: "more", label: "More", icon: Ellipsis },
  ];

  return (
    <div className="flex h-dvh bg-background text-foreground">
      <Sidebar tabs={tabs} more={more} tab={tab} setTab={setTab} />
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex items-center gap-3 border-b px-4 pt-[calc(env(safe-area-inset-top)+0.6rem)] pb-2.5 md:px-6 md:py-3">
          <span className={cn("size-2.5 shrink-0 rounded-full md:hidden", connected ? "bg-emerald-500" : "bg-red-500")} />
          <div className="min-w-0 flex-1">
            <div className="font-semibold leading-tight md:hidden">lyra</div>
            {status.title && <div className="truncate text-muted-foreground text-xs md:text-foreground md:text-sm">{status.title}</div>}
          </div>
          {ready && (
            <Badge
              variant="secondary"
              className={cn(
                "max-w-[48vw] truncate",
                asking && "bg-amber-500/20 text-amber-300",
                working && !asking && "bg-sky-500/20 text-sky-300",
                status.waiting && !asking && !working && "bg-teal-500/20 text-teal-300",
              )}
            >
              {status.phase}
            </Badge>
          )}
        </header>
        {banner && (
          <Alert className="rounded-none border-x-0 border-t-0 bg-amber-950/40 py-2">
            <WifiOff />
            <AlertDescription className="text-amber-300">{banner}</AlertDescription>
          </Alert>
        )}
        {newer && (
          <div className="flex items-center justify-between gap-3 border-b bg-teal-950/40 px-4 py-2 text-sm text-teal-200">
            <span className="flex items-center gap-2">
              <Sparkles className="size-4" /> A new version of the lyra app is ready.
            </span>
            <Button size="sm" onClick={updateApp} className="bg-teal-500 text-black hover:bg-teal-400">
              Update
            </Button>
          </div>
        )}

        <main className="flex min-h-0 flex-1 flex-col">
          {tab === "chat" && <ChatPage />}
          {tab === "machines" && <MachinesPage mention={mention} />}
          {tab === "devices" && <DevicesPage />}
          {tab === "activity" && <ActivityPage />}
          {tab === "more" && <MorePage toChat={() => setTab("chat")} open={setTab} update={updateApp} />}
          {tab === "memory" && <MemoryPage onBack={toMore} />}
          {tab === "skills" && <SkillsPage onBack={toMore} />}
          {tab === "goals" && <GoalsPage onBack={toMore} />}
          {tab === "model" && <ModelsPage onBack={toMore} />}
        </main>

        <nav className="grid grid-cols-5 border-t bg-card/60 pb-[env(safe-area-inset-bottom)] backdrop-blur md:hidden">
          {tabs.map((t) => (
            <button
              key={t.id}
              type="button"
              onClick={() => setTab(t.id)}
              className={cn("relative flex flex-col items-center gap-0.5 pt-2 pb-2 text-[11px]", tab === t.id || (t.id === "more" && manage.includes(tab as Manage)) ? "text-teal-400" : "text-muted-foreground")}
            >
              <t.icon className="size-5" />
              {t.label}
              {!!t.badge && <span className="absolute top-1 right-[calc(50%-1.4rem)] min-w-4 rounded-full bg-amber-400 px-1 font-semibold text-[10px] text-black">{t.badge}</span>}
            </button>
          ))}
        </nav>
      </div>
    </div>
  );
}

function Pair({ onPaired, message }: { onPaired: (token: string) => void; message?: string }) {
  const ua = navigator.userAgent;
  // Opened from `lyra pair`'s link or QR code: the code is already filled in.
  const [code, setCode] = useState(() => new URLSearchParams(location.search).get("pair") ?? "");
  useEffect(() => {
    if (new URLSearchParams(location.search).has("pair")) history.replaceState(null, "", "/");
  }, []);
  const [name, setName] = useState(/iphone/i.test(ua) ? "iPhone" : /ipad/i.test(ua) ? "iPad" : /android/i.test(ua) ? "Android" : "Browser");
  const [error, setError] = useState(message ?? "");
  const [busy, setBusy] = useState(false);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError("");
    const r = await fetch("/api/pair", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code, name }),
    });
    const body = await r.json().catch(() => ({}));
    setBusy(false);
    if (!r.ok) {
      setError(body.error || "Pairing failed");
      return;
    }
    saveToken(body.token);
    onPaired(body.token);
  };
  return (
    <div className="flex min-h-dvh items-center justify-center bg-background p-5 text-foreground">
      <Card className="w-full max-w-sm">
        <CardHeader className="items-center text-center">
          <img src="/icon-192.png" alt="" className="mx-auto mb-2 size-16 rounded-2xl" />
          <CardTitle className="text-xl">Pair this device</CardTitle>
          <CardDescription>
            On the computer running lyra, run <code className="rounded bg-muted px-1">lyra pair</code> (or <code className="rounded bg-muted px-1">lyra-pair</code> on the server) and enter the code it
            shows, or scan its QR code.
          </CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={submit} className="space-y-3">
            <Input
              value={code}
              onChange={(e) => setCode(e.target.value)}
              placeholder="Pairing code"
              autoCapitalize="characters"
              spellCheck={false}
              autoComplete="one-time-code"
              className="h-12 text-center font-mono text-lg tracking-[0.2em]"
              required
            />
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="Device name" className="h-11 text-center" />
            <Button type="submit" disabled={busy} className="h-11 w-full">
              Pair
            </Button>
            {error && <p className="text-center text-red-400 text-sm">{error}</p>}
          </form>
        </CardContent>
      </Card>
    </div>
  );
}

export default function App() {
  const [token, setToken] = useState<string | null>(() => loadToken());
  const [why, setWhy] = useState<string | undefined>();
  const unpaired = useCallback((message?: string) => {
    setWhy(message);
    setToken(null);
  }, []);
  if (!token) return <Pair onPaired={setToken} message={why} />;
  return (
    <LyraProvider token={token} onUnpaired={unpaired}>
      <Shell />
    </LyraProvider>
  );
}
