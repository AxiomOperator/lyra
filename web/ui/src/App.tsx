// lyra's web app: pair once, then chat, machines, devices, activity, more.

import { Alert, AlertDescription } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuLabel, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { Separator } from "@/components/ui/separator";
import {
  Sidebar,
  SidebarContent,
  SidebarFooter,
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarMenu,
  SidebarMenuBadge,
  SidebarMenuButton,
  SidebarMenuItem,
  SidebarProvider,
  SidebarTrigger,
  useSidebar,
} from "@/components/ui/sidebar";
import { TooltipProvider } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import {
  Activity,
  AlarmClock,
  Bell,
  Brain,
  Cpu,
  Ellipsis,
  EllipsisVertical,
  RefreshCw,
  GraduationCap,
  MessageSquare,
  MessageSquarePlus,
  ScrollText,
  Server,
  Smartphone,
  Sparkles,
  Target,
  WifiOff,
} from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { ChatPage } from "./lyra/chat";
import { GoalsPage, MemoryPage, ModelsPage, RoutinesPage, SkillsPage } from "./lyra/manage";
import { StatusPage } from "./lyra/status";
import { ActivityPage, DevicesPage, MachinesPage, MorePage } from "./lyra/pages";
import { ago } from "./lyra/push";
import { SearchBox, SearchHits, useConversationSearch } from "./lyra/search";
import { APP_VERSION, LyraProvider, useData, useLyra } from "./lyra/store";
import type { Session } from "./lyra/types";
import { loadToken, saveToken, takeShared } from "./lyra/token";

type Tab = "chat" | "status" | "machines" | "devices" | "activity" | "more" | Manage;

/** Pages reached from More on a phone, and listed in the sidebar on a wide screen. */
type Manage = "routines" | "memory" | "skills" | "goals" | "model";
const manage: Manage[] = ["routines", "memory", "skills", "goals", "model"];

type TabItem = {
  id: Tab;
  label: string;
  icon: typeof MessageSquare;
  badge?: number;
};

/** The sidebar (dashboard-01's inset style): navigation, lyra's pages,
 *  conversations, and this device at the bottom. A sheet on a phone. */
function AppSidebar({ tabs, more, tab, setTab, update }: { tabs: TabItem[]; more: TabItem[]; tab: Tab; setTab: (t: Tab) => void; update: () => void }) {
  const { connected, status, say, device } = useLyra();
  const { setOpenMobile } = useSidebar();
  // Refreshed as conversations start, finish or get a title, here or on another device.
  const live = status.conversations ?? [];
  const [sessions] = useData<Session[]>("sessions", [status.title, JSON.stringify(live)]);
  const answering = (id: string) => live.some((c) => c.session === id && c.answering);
  const go = (t: Tab) => {
    setTab(t);
    setOpenMobile(false);
  };
  const resume = (id: string, current: boolean) => {
    if (!current) say(`/resume ${id}`);
    go("chat");
  };
  const search = useConversationSearch();
  const item = (t: TabItem) => (
    <SidebarMenuItem key={t.id}>
      <SidebarMenuButton tooltip={t.label} isActive={tab === t.id} onClick={() => go(t.id)}>
        <t.icon />
        <span>{t.label}</span>
      </SidebarMenuButton>
      {!!t.badge && (
        <SidebarMenuBadge className="rounded-full bg-amber-400 font-semibold text-black peer-hover/menu-button:text-black peer-data-[active=true]/menu-button:text-black">{t.badge}</SidebarMenuBadge>
      )}
    </SidebarMenuItem>
  );
  return (
    <Sidebar collapsible="offcanvas" variant="inset">
      <SidebarHeader>
        <SidebarMenu>
          <SidebarMenuItem>
            <SidebarMenuButton className="data-[slot=sidebar-menu-button]:p-1.5!" onClick={() => go("chat")}>
              <img src="/icon-192.png" alt="" className="size-5! rounded" />
              <span className="font-semibold text-base">lyra</span>
              <span title={connected ? "connected" : "not connected"} className={cn("ml-auto size-2 rounded-full", connected ? "bg-emerald-500" : "bg-red-500")} />
            </SidebarMenuButton>
          </SidebarMenuItem>
        </SidebarMenu>
        <SearchBox query={search.query} setQuery={search.setQuery} />
      </SidebarHeader>
      <SidebarContent>
        {/* Searching: the results take the sidebar until the box is cleared. */}
        {search.hits && (
          <SidebarGroup className="min-h-0 flex-1">
            <SidebarGroupLabel>Conversations mentioning “{search.query.trim()}”</SidebarGroupLabel>
            <SidebarGroupContent className="min-h-0 flex-1 overflow-y-auto">
              <SearchHits hits={search.hits} open={resume} />
            </SidebarGroupContent>
          </SidebarGroup>
        )}
        {!search.hits && (
          <>
            <SidebarGroup>
              <SidebarGroupContent className="flex flex-col gap-2">
                <SidebarMenu>
                  <SidebarMenuItem>
                    <SidebarMenuButton
                      tooltip="New conversation"
                      className="min-w-8 bg-primary text-primary-foreground duration-200 ease-linear hover:bg-primary/90 hover:text-primary-foreground active:bg-primary/90 active:text-primary-foreground"
                      onClick={() => {
                        say("/new");
                        go("chat");
                      }}
                    >
                      <MessageSquarePlus />
                      <span>New conversation</span>
                    </SidebarMenuButton>
                  </SidebarMenuItem>
                </SidebarMenu>
                <SidebarMenu>{tabs.filter((t) => t.id !== "more").map(item)}</SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
            <SidebarGroup>
              <SidebarGroupLabel>Lyra</SidebarGroupLabel>
              <SidebarMenu>{more.map(item)}</SidebarMenu>
            </SidebarGroup>
            <SidebarGroup className="min-h-0 flex-1">
              <SidebarGroupLabel>Conversations</SidebarGroupLabel>
              <SidebarGroupContent className="min-h-0 flex-1 overflow-y-auto">
                <SidebarMenu>
                  {(sessions ?? []).slice(0, 40).map((s) => (
                    <SidebarMenuItem key={s.id}>
                      <SidebarMenuButton size="lg" isActive={s.current && tab === "chat"} onClick={() => resume(s.id, s.current)} className="h-auto py-1.5">
                        <span className="grid min-w-0 flex-1 leading-tight">
                          <span className="truncate text-sm">{s.title || "(untitled)"}</span>
                          <span className="truncate text-muted-foreground text-xs">{ago(s.updated)}</span>
                        </span>
                        {answering(s.id) && <span title="answering" className="size-2 shrink-0 animate-pulse rounded-full bg-sky-400" />}
                      </SidebarMenuButton>
                    </SidebarMenuItem>
                  ))}
                </SidebarMenu>
              </SidebarGroupContent>
            </SidebarGroup>
          </>
        )}
      </SidebarContent>
      <SidebarFooter>
        <SidebarMenu>
          <SidebarMenuItem>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <SidebarMenuButton size="lg" className="data-[state=open]:bg-sidebar-accent data-[state=open]:text-sidebar-accent-foreground">
                  <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-sidebar-accent font-semibold text-xs uppercase">{(device?.name ?? "?").slice(0, 2)}</span>
                  <span className="grid flex-1 text-left text-sm leading-tight">
                    <span className="truncate font-medium">{device?.name ?? "this device"}</span>
                    <span className="truncate text-muted-foreground text-xs">{status.model}</span>
                  </span>
                  <EllipsisVertical className="ml-auto size-4" />
                </SidebarMenuButton>
              </DropdownMenuTrigger>
              <DropdownMenuContent className="w-(--radix-dropdown-menu-trigger-width) min-w-56 rounded-lg" side="right" align="end" sideOffset={4}>
                <DropdownMenuLabel className="font-normal">
                  <div className="grid text-xs leading-tight">
                    <span className="font-medium text-sm">{device?.name}</span>
                    <span className="text-muted-foreground">model {status.model}</span>
                    {status.decide && (
                      <span className="text-muted-foreground">
                        decides: {status.decide.model} · {status.decide.ms} ms
                      </span>
                    )}
                    <span className="text-muted-foreground">app {APP_VERSION}</span>
                  </div>
                </DropdownMenuLabel>
                <DropdownMenuSeparator />
                <DropdownMenuItem onClick={() => go("devices")}>
                  <Smartphone /> Devices
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => go("more")}>
                  <Bell /> Notifications & more
                </DropdownMenuItem>
                <DropdownMenuItem onClick={update}>
                  <RefreshCw /> Update the app
                </DropdownMenuItem>
              </DropdownMenuContent>
            </DropdownMenu>
          </SidebarMenuItem>
        </SidebarMenu>
      </SidebarFooter>
    </Sidebar>
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
  // Machines (and the server) with a health problem.
  const troubled = (status.machines_detail ?? []).filter((m) => m.online && m.health?.problems.length).length + (status.server_health?.problems.length ? 1 : 0);
  const newer = !!serverVersion && serverVersion !== APP_VERSION;
  const asking = (status.approvals?.length ?? 0) > 0;
  const working = status.phase?.startsWith("↪");

  const mention = (name: string) => {
    setTab("chat");
    // The composer picks it up from here.
    setTimeout(() => window.dispatchEvent(new CustomEvent("lyra-mention", { detail: name })), 0);
  };

  const more: TabItem[] = [
    { id: "routines", label: "Routines", icon: AlarmClock, badge: (status.routines ?? []).filter((r) => r.runs[0]?.needs_user).length },
    { id: "memory", label: "Memory", icon: Brain },
    { id: "skills", label: "Skills", icon: GraduationCap },
    { id: "goals", label: "Goals", icon: Target },
    { id: "model", label: "Model", icon: Cpu },
  ];
  const toMore = () => setTab("more");
  const tabs: TabItem[] = [
    { id: "chat", label: "Chat", icon: MessageSquare, badge: asking ? 1 : 0 },
    { id: "status", label: "Status", icon: Activity, badge: (status.status?.rows ?? []).filter((r) => r.state === "down" && r.group !== "Machines").length },
    {
      id: "machines",
      label: "Machines",
      icon: Server,
      badge: pairing + updates + troubled,
    },
    { id: "devices", label: "Devices", icon: Smartphone, badge: pairing },
    { id: "activity", label: "Activity", icon: ScrollText },
    { id: "more", label: "More", icon: Ellipsis },
  ];

  const title = [...tabs, ...more].find((t) => t.id === tab)?.label ?? "lyra";
  return (
    <SidebarProvider
      className="h-dvh min-h-0 bg-sidebar text-foreground"
      style={{ "--sidebar-width": "calc(var(--spacing) * 72)", "--header-height": "calc(var(--spacing) * 12)" } as React.CSSProperties}
    >
      <AppSidebar tabs={tabs} more={more} tab={tab} setTab={setTab} update={updateApp} />
      <SidebarInset className="min-h-0 min-w-0 overflow-hidden">
        {/* dashboard-01's site header: the sidebar toggle, the page, what lyra is doing. */}
        <header className="flex shrink-0 items-center gap-2 border-b pt-[env(safe-area-inset-top)] md:h-(--header-height) md:pt-0">
          <div className="flex w-full items-center gap-1 px-4 py-2 md:py-0 lg:gap-2 lg:px-6">
            <SidebarTrigger className="-ml-1" />
            <Separator orientation="vertical" className="mx-2 data-[orientation=vertical]:h-4" />
            <span className={cn("size-2 shrink-0 rounded-full md:hidden", connected ? "bg-emerald-500" : "bg-red-500")} />
            <div className="min-w-0 flex-1">
              <h1 className="truncate font-medium text-base leading-tight">{tab === "chat" ? status.title || "lyra" : title}</h1>
            </div>
            {ready && (
              <Badge
                variant="outline"
                className={cn(
                  "max-w-[48vw] truncate",
                  asking && "border-amber-500/50 bg-amber-500/15 text-amber-300",
                  working && !asking && "border-sky-500/50 bg-sky-500/15 text-sky-300",
                  status.waiting && !asking && !working && "border-teal-500/50 bg-teal-500/15 text-teal-300",
                )}
              >
                {status.phase}
              </Badge>
            )}
          </div>
        </header>
        {banner && (
          <Alert className="rounded-none border-x-0 border-t-0 bg-amber-950/40 py-2">
            <WifiOff />
            <AlertDescription className="text-amber-300">{banner}</AlertDescription>
          </Alert>
        )}
        {newer && (
          <div className="flex items-center justify-between gap-3 border-b bg-teal-950/40 px-4 py-2 text-sm text-teal-200 lg:px-6">
            <span className="flex items-center gap-2">
              <Sparkles className="size-4" /> A new version of the lyra app is ready.
            </span>
            <Button size="sm" onClick={updateApp} className="bg-teal-500 text-black hover:bg-teal-400">
              Update
            </Button>
          </div>
        )}

        <div className="flex min-h-0 flex-1 flex-col">
          {tab === "chat" && <ChatPage />}
          {tab === "status" && <StatusPage toMachines={() => setTab("machines")} toChat={() => setTab("chat")} />}
          {tab === "machines" && <MachinesPage mention={mention} toStatus={() => setTab("status")} toChat={() => setTab("chat")} />}
          {tab === "devices" && <DevicesPage />}
          {tab === "activity" && <ActivityPage />}
          {tab === "more" && <MorePage toChat={() => setTab("chat")} open={setTab} update={updateApp} />}
          {tab === "routines" && <RoutinesPage onBack={toMore} toChat={() => setTab("chat")} />}
          {tab === "memory" && <MemoryPage onBack={toMore} />}
          {tab === "skills" && <SkillsPage onBack={toMore} />}
          {tab === "goals" && <GoalsPage onBack={toMore} />}
          {tab === "model" && <ModelsPage onBack={toMore} />}
        </div>

        <nav className="grid grid-cols-5 border-t bg-card/60 pb-[env(safe-area-inset-bottom)] backdrop-blur md:hidden">
          {/* Five fit: Devices lives under More on a phone. */}
          {tabs
            .filter((t) => t.id !== "devices")
            .map((t) => (
              <button
                key={t.id}
                type="button"
                onClick={() => setTab(t.id)}
                className={cn(
                  "relative flex flex-col items-center gap-0.5 pt-2 pb-2 text-[11px]",
                  tab === t.id || (t.id === "more" && (manage.includes(tab as Manage) || tab === "devices")) ? "text-teal-400" : "text-muted-foreground",
                )}
              >
                <t.icon className="size-5" />
                {t.label}
                {!!t.badge && <span className="absolute top-1 right-[calc(50%-1.4rem)] min-w-4 rounded-full bg-amber-400 px-1 font-semibold text-[10px] text-black">{t.badge}</span>}
              </button>
            ))}
        </nav>
      </SidebarInset>
    </SidebarProvider>
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
      <TooltipProvider>
        <Shell />
      </TooltipProvider>
    </LyraProvider>
  );
}
