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
  SidebarGroup,
  SidebarGroupContent,
  SidebarGroupLabel,
  SidebarHeader,
  SidebarInset,
  SidebarProvider,
  SidebarTrigger,
  useSidebar,
} from "@/components/ui/sidebar";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import {
  Activity,
  Code2,
  AlarmClock,
  ListTodo,
  NotebookPen,
  Users,
  Gauge,
  FolderOpen,
  Bell,
  Brain,
  Cpu,
  Ellipsis,
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
  Search,
} from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { ChatPage } from "./lyra/chat";
import { ConversationList } from "./lyra/conversations";
import { EverythingSearch } from "./lyra/everything";
import { UpdatedNote, WhatsNewPage } from "./lyra/whatsnew";
import { GoalsPage, MemoryPage, ModelsPage, RoutinesPage, SkillsPage } from "./lyra/manage";
import { TasksPage } from "./lyra/tasks";
import { NotesPage } from "./lyra/notes";
import { UsersPage } from "./lyra/users";
import { UsagePage } from "./lyra/usage";
import { ProjectsPage } from "./lyra/projects";
import { StatusPage } from "./lyra/status";
import { CodingPage } from "./lyra/coding";
import { ActivityPage, DevicesPage, MachinesPage, MorePage } from "./lyra/pages";
import { SearchBox, SearchHits, useConversationSearch } from "./lyra/search";
import { APP_VERSION, LyraProvider, useData, useLyra } from "./lyra/store";
import type { Session } from "./lyra/types";
import { loadToken, saveToken, takeShared } from "./lyra/token";

type Tab = "chat" | "status" | "machines" | "devices" | "activity" | "more" | Manage;

/** Pages reached from More on a phone, and listed in the sidebar on a wide screen. */
type Manage = "whatsnew" | "tasks" | "notes" | "projects" | "routines" | "coding" | "memory" | "skills" | "goals" | "model" | "usage" | "users";
const manage: Manage[] = ["whatsnew", "tasks", "notes", "projects", "routines", "coding", "memory", "skills", "goals", "model", "usage", "users"];
/** What a member (not an admin) has: their chats, tasks, status, activity, skills. */
const forMembers: string[] = ["chat", "status", "activity", "more", "skills", "whatsnew", "tasks", "notes", "projects", "routines", "goals", "memory", "usage"];

type TabItem = {
  id: Tab;
  label: string;
  icon: typeof MessageSquare;
  badge?: number;
};

/** The icon rail's pages, in groups (a line between them). */
const railGroups: Tab[][] = [
  ["chat", "status", "activity"],
  ["tasks", "notes", "projects", "routines", "goals"],
  ["memory", "skills", "coding", "model", "whatsnew"],
  ["machines", "devices", "users", "usage"],
];

/** The sidebar (dashboard-01's inset style): an icon rail of lyra's pages
 *  on the left, the conversations beside it, this device at the bottom of
 *  the rail. A sheet on a phone. */
function AppSidebar({ tabs, more, tab, setTab, update }: { tabs: TabItem[]; more: TabItem[]; tab: Tab; setTab: (t: Tab) => void; update: () => void }) {
  const { connected, status, say, device, user } = useLyra();
  const { setOpenMobile, isMobile } = useSidebar();
  // Tooltips on hover only (a phone would pop one up as the sheet opens).
  const tip = (label: string, child: React.ReactElement, key?: string) =>
    isMobile ? (
      <span key={key} className="contents">
        {child}
      </span>
    ) : (
      <Tooltip key={key}>
        <TooltipTrigger asChild>{child}</TooltipTrigger>
        <TooltipContent side="right">{label}</TooltipContent>
      </Tooltip>
    );
  // Refreshed as conversations start, finish or get a title, here or on another device.
  const live = status.conversations ?? [];
  const [sessionList, reloadSessions] = useData<Session[]>("sessions", [status.title, JSON.stringify(live)]);
  // "Answering" comes live from the status, not the list.
  const sessions = (sessionList ?? []).map((s) => ({ ...s, answering: live.some((c) => c.session === s.id && c.answering) }));
  const go = (t: Tab) => {
    setTab(t);
    setOpenMobile(false);
  };
  const resume = (id: string, current: boolean) => {
    if (!current) say(`/resume ${id}`);
    go("chat");
  };
  const search = useConversationSearch();
  const all = [...tabs, ...more];
  const groups = railGroups.map((g) => g.map((id) => all.find((t) => t.id === id)).filter((t): t is TabItem => !!t)).filter((g) => g.length > 0);
  // Each is named under its icon: no tooltip needed.
  const plain = (child: React.ReactElement, key: string) => (
    <span key={key} className="contents">
      {child}
    </span>
  );
  const railItem = (t: TabItem) =>
    plain(
      <button
        type="button"
        aria-label={t.label}
        onClick={() => go(t.id)}
        className={cn(
          "relative flex shrink-0 items-center justify-center rounded-md text-sidebar-foreground/70 transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground [&>svg]:size-[18px]",
          // A phone has no hover: each icon says what it is.
          // Each icon says what it is, on a phone and on a desktop.
          "w-14 flex-col gap-0.5 py-1",
          tab === t.id && "bg-primary/15 text-primary hover:bg-primary/20 hover:text-primary",
        )}
      >
        <t.icon />
        <span className="w-full truncate text-center text-[10px] leading-tight">{t.label}</span>
        {!!t.badge && <span className="absolute top-0.5 right-2 min-w-4 rounded-full bg-amber-400 px-1 text-center font-semibold text-[10px] text-black leading-4">{t.badge}</span>}
      </button>,
      t.id,
    );
  return (
    <Sidebar collapsible="offcanvas" variant="inset">
      <div className="flex h-full min-h-0">
        {/* The rail: lyra's pages, with what waits on each. */}
        <nav className="flex w-16 shrink-0 flex-col items-center gap-1 py-2">
          {tip(
            connected ? "lyra · connected" : "lyra · not connected",
            <button type="button" aria-label="lyra" onClick={() => go("chat")} className="relative mb-1 flex size-9 items-center justify-center">
              <img src="/icon-192.png" alt="" className="size-6 rounded" />
              <span className={cn("absolute right-0.5 bottom-0.5 size-2 rounded-full ring-2 ring-sidebar", connected ? "bg-emerald-500" : "bg-red-500")} />
            </button>,
          )}
          {/* Scrolls on a short screen, never sideways, without a bar. */}
          <div className="flex min-h-0 w-full flex-1 flex-col items-center gap-1 overflow-x-hidden overflow-y-auto pt-0.5 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
            {groups.map((g, i) => (
              <div key={i} className={cn("flex flex-col items-center", i > 0 && "mt-0.5 border-sidebar-border border-t pt-1")}>
                {g.map(railItem)}
              </div>
            ))}
          </div>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <button type="button" aria-label="This device" className="mt-1 flex size-9 items-center justify-center rounded-lg bg-sidebar-accent font-semibold text-xs uppercase">
                {(user?.name ?? device?.name ?? "?").slice(0, 2)}
              </button>
            </DropdownMenuTrigger>
            <DropdownMenuContent className="min-w-56 rounded-lg" side="right" align="end" sideOffset={4}>
              <DropdownMenuLabel className="font-normal">
                <div className="grid text-xs leading-tight">
                  <span className="font-medium text-sm">{user?.name ?? device?.name ?? "this device"}</span>
                  {user && (
                    <span className="text-muted-foreground">
                      {user.admin ? "admin" : "member"} · {device?.name}
                    </span>
                  )}
                  <span className="text-muted-foreground">model {status.model}</span>
                  {status.decide && (
                    <span className="text-muted-foreground">
                      decides: {status.decide.model} · {status.decide.ms} ms
                    </span>
                  )}
                  <span className="text-muted-foreground">lyra {status.version ?? "…"}</span>
                  <span className="text-muted-foreground">app {APP_VERSION}</span>
                </div>
              </DropdownMenuLabel>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => go("whatsnew")}>
                <Sparkles /> What's new
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => go("more")}>
                <Bell /> Notifications & more
              </DropdownMenuItem>
              <DropdownMenuItem onClick={update}>
                <RefreshCw /> Update the app
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </nav>
        {/* The conversations. */}
        <div className="flex min-h-0 min-w-0 flex-1 flex-col">
          <SidebarHeader className="gap-2">
            {/* Quiet: a teal outline, filled only on hover. */}
            <Button
              variant="outline"
              className="w-full justify-start border-primary/40 bg-primary/10 text-primary hover:bg-primary/20 hover:text-primary"
              onClick={() => {
                say("/new");
                go("chat");
              }}
            >
              <MessageSquarePlus /> New conversation
            </Button>
            <SearchBox query={search.query} setQuery={search.setQuery} />
          </SidebarHeader>
          <SidebarContent>
            {/* Searching: the results take the panel until the box is cleared. */}
            {search.hits ? (
              <SidebarGroup className="min-h-0 flex-1">
                <SidebarGroupLabel>Conversations mentioning “{search.query.trim()}”</SidebarGroupLabel>
                <SidebarGroupContent className="min-h-0 flex-1 overflow-y-auto">
                  <SearchHits hits={search.hits} open={resume} />
                </SidebarGroupContent>
              </SidebarGroup>
            ) : (
              <ConversationList sessions={sessions} active={tab === "chat"} open={resume} reload={reloadSessions} />
            )}
          </SidebarContent>
        </div>
      </div>
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
  const { status, connected, banner, serverVersion, ready, user } = useLyra();
  const admin = user?.admin ?? true;
  // Opened from Android's share sheet: hand what was shared to the composer.
  useEffect(() => {
    if (!ready || !new URLSearchParams(location.search).has("shared")) return;
    history.replaceState(null, "", "/");
    void takeShared().then((shared) => {
      if (shared) window.dispatchEvent(new CustomEvent("lyra-share", { detail: shared }));
    });
  }, [ready]);
  const [tab, setTab] = useState<Tab>("chat");
  // One search for everything: Ctrl-K / ⌘K, or the header's search button.
  const [searching, setSearching] = useState(false);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setSearching((s) => !s);
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, []);
  // Opened from a notification for a page (the briefing → Status): go there.
  useEffect(() => {
    const go = (page: string | null) => {
      if (page && (["status", "machines", "activity", "more", ...manage] as string[]).includes(page)) setTab(page as Tab);
    };
    const asked = new URLSearchParams(location.search).get("page");
    if (asked) {
      history.replaceState(null, "", "/");
      go(asked);
    }
    const onMessage = (e: MessageEvent) => go(e.data?.page ?? null);
    navigator.serviceWorker?.addEventListener("message", onMessage);
    return () => navigator.serviceWorker?.removeEventListener("message", onMessage);
  }, []);
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

  const day = new Date();
  const todayKey = `${day.getFullYear()}-${String(day.getMonth() + 1).padStart(2, "0")}-${String(day.getDate()).padStart(2, "0")}`;
  const pmiWaiting = (status.pmi?.waiting.task_transfers?.length ?? 0) + (status.pmi?.waiting.project_transfers?.length ?? 0) + (status.pmi?.waiting.approvals?.length ?? 0);
  const more: TabItem[] = ([
    { id: "tasks", label: "Tasks", icon: ListTodo, badge: (status.pmi?.tasks ?? []).filter((t) => t.due && t.due < todayKey).length + pmiWaiting },
    { id: "notes", label: "Notes", icon: NotebookPen },
    { id: "whatsnew", label: "What's new", icon: Sparkles },
    { id: "projects", label: "Projects", icon: FolderOpen },
    { id: "routines", label: "Routines", icon: AlarmClock, badge: (status.routines ?? []).filter((r) => r.runs[0]?.needs_user).length },
    { id: "coding", label: "Coding", icon: Code2 },
    { id: "memory", label: "Memory", icon: Brain },
    { id: "skills", label: "Skills", icon: GraduationCap },
    { id: "goals", label: "Goals", icon: Target },
    { id: "model", label: "Model", icon: Cpu },
    { id: "usage", label: "Usage", icon: Gauge },
    { id: "users", label: "Users", icon: Users, badge: status.users_waiting ?? 0 },
  ] as TabItem[]).filter((t) => admin || forMembers.includes(t.id));
  const toMore = () => setTab("more");
  const tabs: TabItem[] = ([
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
  ] as TabItem[]).filter((t) => admin || forMembers.includes(t.id));

  const title = [...tabs, ...more].find((t) => t.id === tab)?.label ?? "lyra";
  return (
    <SidebarProvider
      className="h-dvh min-h-0 bg-sidebar text-foreground"
      style={{ "--sidebar-width": "calc(var(--spacing) * 88)", "--header-height": "calc(var(--spacing) * 12)" } as React.CSSProperties}
    >
      <AppSidebar tabs={tabs} more={more} tab={tab} setTab={setTab} update={updateApp} />
      <SidebarInset className="min-h-0 min-w-0 overflow-hidden pb-[env(safe-area-inset-bottom)] md:pb-0">
        {/* dashboard-01's site header: the sidebar toggle, the page, what lyra is doing. */}
        <header className="flex shrink-0 items-center gap-2 border-b pt-[env(safe-area-inset-top)] md:h-(--header-height) md:pt-0">
          <div className="flex w-full items-center gap-1 px-4 py-2 md:py-0 lg:gap-2 lg:px-6">
            {/* On a phone the pages are in the sidebar: a dot here says something waits there. */}
            <span className="relative -ml-1">
              <SidebarTrigger />
              {[...tabs, ...more].some((t) => (t.badge ?? 0) > 0) && <span className="pointer-events-none absolute top-1 right-1 size-2 rounded-full bg-amber-400 md:hidden" />}
            </span>
            <Separator orientation="vertical" className="mx-2 data-[orientation=vertical]:h-4" />
            <span className={cn("size-2 shrink-0 rounded-full md:hidden", connected ? "bg-emerald-500" : "bg-red-500")} />
            <div className="min-w-0 flex-1">
              <h1 className="truncate font-medium text-base leading-tight">{tab === "chat" ? status.title || "lyra" : title}</h1>
            </div>
            <Button variant="ghost" size="sm" className="gap-1.5 text-muted-foreground" onClick={() => setSearching(true)} aria-label="Search everything">
              <Search className="size-4" />
              <span className="hidden text-xs md:inline">Search</span>
              <kbd className="hidden rounded border px-1 font-mono text-[10px] md:inline">Ctrl K</kbd>
            </Button>
            <EverythingSearch open={searching} setOpen={setSearching} go={(p) => setTab(p as Tab)} />
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
        {!newer && (
          <div className="px-3">
            <UpdatedNote open={() => setTab("whatsnew")} />
          </div>
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
          {tab === "status" && <StatusPage toMachines={() => setTab("machines")} toChat={() => setTab("chat")} go={(p) => setTab(p as Tab)} />}
          {tab === "machines" && <MachinesPage mention={mention} toStatus={() => setTab("status")} toChat={() => setTab("chat")} />}
          {tab === "devices" && <DevicesPage />}
          {tab === "activity" && <ActivityPage />}
          {tab === "more" && <MorePage toChat={() => setTab("chat")} open={setTab} update={updateApp} />}
          {tab === "tasks" && <TasksPage onBack={toMore} />}
          {tab === "notes" && <NotesPage onBack={toMore} />}
          {tab === "whatsnew" && <WhatsNewPage onBack={toMore} />}
          {tab === "projects" && <ProjectsPage onBack={toMore} />}
          {tab === "routines" && <RoutinesPage onBack={toMore} toChat={() => setTab("chat")} />}
          {tab === "coding" && <CodingPage onBack={toMore} />}
          {tab === "memory" && <MemoryPage onBack={toMore} />}
          {tab === "skills" && <SkillsPage onBack={toMore} />}
          {tab === "goals" && <GoalsPage onBack={toMore} />}
          {tab === "model" && <ModelsPage onBack={toMore} />}
          {tab === "usage" && <UsagePage onBack={toMore} />}
          {tab === "users" && <UsersPage onBack={toMore} />}
        </div>

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
  // Microsoft sign-in, when the server has it: then pairing codes are the other way in.
  const [entra, setEntra] = useState(false);
  const [withCode, setWithCode] = useState(() => new URLSearchParams(location.search).has("pair"));
  useEffect(() => {
    void fetch("/api/auth")
      .then((r) => r.json())
      .then((b) => setEntra(!!b.entra))
      .catch(() => {});
  }, []);
  const microsoft = () => {
    location.href = `/auth/login?device=${encodeURIComponent(name)}`;
  };
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
          <CardTitle className="text-xl">{entra && !withCode ? "Sign in to lyra" : "Pair this device"}</CardTitle>
          <CardDescription>
            {entra && !withCode ? (
              "Use your organization's Microsoft account."
            ) : (
              <>
                On the computer running lyra, run <code className="rounded bg-muted px-1">lyra pair</code> (or <code className="rounded bg-muted px-1">lyra-pair</code> on the server) and enter the code it
                shows, or scan its QR code.
              </>
            )}
          </CardDescription>
        </CardHeader>
        <CardContent>
          {entra && !withCode && (
            <div className="space-y-3">
              <Button onClick={microsoft} className="h-11 w-full">
                Sign in with Microsoft
              </Button>
              <button type="button" onClick={() => setWithCode(true)} className="w-full text-center text-muted-foreground text-sm hover:text-foreground">
                Pair with a code instead
              </button>
              {error && <p className="text-center text-red-400 text-sm">{error}</p>}
            </div>
          )}
          <form onSubmit={submit} className={cn("space-y-3", entra && !withCode && "hidden")}>
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

/** Signed in, but an admin hasn't let this account in yet: wait and look again. */
function Waiting({ onIn, onOther }: { onIn: () => void; onOther: () => void }) {
  useEffect(() => {
    const look = () =>
      void fetch("/api/me", { headers: { Authorization: "Bearer " + (loadToken() ?? "") } }).then((r) => {
        if (r.ok) onIn();
      });
    const t = window.setInterval(look, 15000);
    return () => window.clearInterval(t);
  }, [onIn]);
  return (
    <div className="flex min-h-dvh items-center justify-center bg-background p-5 text-foreground">
      <Card className="w-full max-w-sm text-center">
        <CardHeader className="items-center">
          <img src="/icon-192.png" alt="" className="mx-auto mb-2 size-16 rounded-2xl" />
          <CardTitle className="text-xl">Almost there</CardTitle>
          <CardDescription>You're signed in. An admin has to let you into lyra; this page opens by itself when they do.</CardDescription>
        </CardHeader>
        <CardContent>
          <button type="button" onClick={onOther} className="text-muted-foreground text-sm hover:text-foreground">
            Use another account
          </button>
        </CardContent>
      </Card>
    </div>
  );
}

/** Back from Microsoft: a one-time code (or what went wrong) is in the
 * fragment. The code is traded for this device's token only together with
 * the sign-in cookie this browser got when it started signing in, so a link
 * made from someone else's sign-in does nothing here. */
function fromSignIn(): { code?: string; error?: string } {
  const h = new URLSearchParams(location.hash.slice(1));
  const code = h.get("signin-code") ?? undefined;
  const error = h.get("signin-error") ?? undefined;
  if (code || error || h.has("signed-in")) history.replaceState(null, "", "/");
  return { code, error };
}

const signIn = fromSignIn();

async function redeem(code: string): Promise<{ token?: string; error?: string }> {
  try {
    const r = await fetch("/api/auth/redeem", { method: "POST", headers: { "Content-Type": "application/json" }, credentials: "same-origin", body: JSON.stringify({ code }) });
    const b = await r.json().catch(() => ({}));
    return r.ok && b.token ? { token: b.token } : { error: b.error || "The sign-in didn't finish: try again." };
  } catch {
    return { error: "Couldn't reach lyra to finish signing in." };
  }
}

export default function App() {
  const [token, setToken] = useState<string | null>(() => loadToken());
  const [why, setWhy] = useState<string | undefined>(signIn.error);
  const [waiting, setWaiting] = useState(false);
  const [finishing, setFinishing] = useState(!!signIn.code);
  useEffect(() => {
    if (!signIn.code) return;
    const code = signIn.code;
    signIn.code = undefined;
    void redeem(code).then((r) => {
      if (r.token) {
        saveToken(r.token);
        setToken(r.token);
      } else setWhy(r.error);
      setFinishing(false);
    });
  }, []);
  const unpaired = useCallback((message?: string) => {
    // An account waiting for an admin keeps its token and waits.
    if (message === "waiting") {
      setWaiting(true);
      setToken(null);
      return;
    }
    setWhy(message);
    setToken(null);
  }, []);
  if (waiting)
    return (
      <Waiting
        onIn={() => {
          setWaiting(false);
          setToken(loadToken());
        }}
        onOther={() => {
          saveToken(null);
          setWaiting(false);
        }}
      />
    );
  if (finishing) return <div className="flex min-h-dvh items-center justify-center bg-background text-muted-foreground text-sm">Signing you in…</div>;
  if (!token) return <Pair onPaired={setToken} message={why} />;
  return (
    <LyraProvider token={token} onUnpaired={unpaired}>
      <TooltipProvider>
        <Shell />
      </TooltipProvider>
    </LyraProvider>
  );
}
