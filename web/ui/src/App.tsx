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
  CalendarDays,
  ChevronDown,
  UserRound,
  ChevronUp,
  Code2,
  AlarmClock,
  ListTodo,
  NotebookPen,
  Users,
  Gauge,
  Settings as SettingsIcon,
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
  MessageSquareWarning,
  CircleHelp,
  Target,
  WifiOff,
  Search,
  Timer,
  Briefcase,
  Zap,
  Shield,
  Pin,
  PinOff,
  Sun,
  Bot,
} from "lucide-react";
import { loadOpen, onOpenChange, setOpen } from "./lyra/rail";
import { lazy, Suspense, useCallback, useEffect, useRef, useState, type ComponentType, type FormEvent, type ReactNode } from "react";
import { ChatPage } from "./lyra/chat";
import { ConversationList } from "./lyra/conversations";
import { EverythingSearch } from "./lyra/everything";
import { UpdatedNote, WhatsNewPage } from "./lyra/whatsnew";
import { CodingPage } from "./lyra/coding";
import { SearchBox, SearchHits, useConversationSearch } from "./lyra/search";
import { APP_VERSION, LyraProvider, useData, useLyra } from "./lyra/store";
import type { Session } from "./lyra/types";
import { loadToken, saveToken, takeShared } from "./lyra/token";
import { takeIntent } from "./lyra/intent";

// Pages other than the chat load when first opened, so a phone's first load
// is only the chat (D-5). Each module becomes its own chunk.
// After an update the old chunks are gone: a page that can't load its chunk
// reloads once to get the new app (not again, so a real outage shows the error).
const RELOADED = "lyra-chunk-reload";
function remember(key: string, value: string | null) {
  try {
    if (value === null) sessionStorage.removeItem(key);
    else sessionStorage.setItem(key, value);
  } catch {
    // Storage blocked: no reload loop guard, so no reload either.
  }
}
function reloadedOnce() {
  try {
    return sessionStorage.getItem(RELOADED) !== null;
  } catch {
    return true;
  }
}
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function page<M extends Record<K, ComponentType<any>>, K extends keyof M>(load: () => Promise<M>, name: K) {
  return lazy(() =>
    load().then(
      (m) => {
        remember(RELOADED, null);
        return { default: m[name] };
      },
      (e) => {
        if (reloadedOnce()) throw e;
        remember(RELOADED, "1");
        location.reload();
        return new Promise<never>(() => {});
      },
    ),
  );
}
const FeedbackPage = page(() => import("./lyra/feedback"), "FeedbackPage");
const QAPage = page(() => import("./lyra/qa"), "QAPage");
const GoalsPage = page(() => import("./lyra/manage"), "GoalsPage");
const MemoryPage = page(() => import("./lyra/manage"), "MemoryPage");
const ModelsPage = page(() => import("./lyra/manage"), "ModelsPage");
const RoutinesPage = page(() => import("./lyra/manage"), "RoutinesPage");
const SkillsPage = page(() => import("./lyra/manage"), "SkillsPage");
const TasksPage = page(() => import("./lyra/tasks"), "TasksPage");
const NotesPage = page(() => import("./lyra/notes"), "NotesPage");
const UsersPage = page(() => import("./lyra/users"), "UsersPage");
const UsagePage = page(() => import("./lyra/usage"), "UsagePage");
const RunningPage = page(() => import("./lyra/running"), "RunningPage");
const ProfilePage = page(() => import("./lyra/profile"), "ProfilePage");
const SettingsPage = page(() => import("./lyra/settings"), "SettingsPage");
const MeetingsPage = page(() => import("./lyra/meetings"), "MeetingsPage");
const AboutMePage = page(() => import("./lyra/aboutme"), "AboutMePage");
const ProjectsPage = page(() => import("./lyra/projects"), "ProjectsPage");
const StatusPage = page(() => import("./lyra/status"), "StatusPage");
const TodayPage = page(() => import("./lyra/today"), "TodayPage");
const AgentsPage = page(() => import("./lyra/agents"), "AgentsPage");
const ActivityPage = page(() => import("./lyra/pages"), "ActivityPage");
const DevicesPage = page(() => import("./lyra/pages"), "DevicesPage");
const MachinesPage = page(() => import("./lyra/pages"), "MachinesPage");
const MorePage = page(() => import("./lyra/pages"), "MorePage");

/** While a page's chunk loads: a quiet line, not a flash of the old page. */
function Loading() {
  return <div className="flex flex-1 items-center justify-center text-muted-foreground text-sm">Loading…</div>;
}

type Tab = "chat" | "status" | "machines" | "devices" | "activity" | "more" | Manage;

/** Pages reached from More on a phone, and listed in the sidebar on a wide screen. */
type Manage = "today" | "agents" | "whatsnew" | "feedback" | "qa" | "tasks" | "notes" | "projects" | "routines" | "coding" | "memory" | "skills" | "goals" | "model" | "usage" | "users" | "settings" | "meetings" | "me" | "running" | "profile";
const manage: Manage[] = ["today", "agents", "me", "whatsnew", "feedback", "qa", "tasks", "notes", "projects", "routines", "coding", "memory", "skills", "goals", "model", "usage", "users", "settings", "meetings", "running", "profile"];
/** What a member (not an admin) has: their chats, day, tasks, activity, skills (Status is admins'). */
const forMembers: string[] = ["chat", "today", "activity", "more", "skills", "whatsnew", "feedback", "qa", "tasks", "notes", "projects", "routines", "goals", "memory", "usage", "meetings", "me", "profile"];

type TabItem = {
  id: Tab;
  label: string;
  icon: typeof MessageSquare;
  badge?: number;
};

/** The icon rail: a few groups, each a small menu of its pages (one page
 *  alone opens at once). Badges add up on the group. */
const railGroups: { id: string; label: string; icon: typeof MessageSquare; pages: Tab[] }[] = [
  { id: "chat", label: "Chat", icon: MessageSquare, pages: ["chat"] },
  { id: "work", label: "Work", icon: Briefcase, pages: ["today", "tasks", "meetings", "notes", "projects"] },
  { id: "knowledge", label: "Knowledge", icon: Brain, pages: ["memory", "skills"] },
  { id: "automation", label: "Automation", icon: Zap, pages: ["agents", "routines", "goals", "coding"] },
  { id: "system", label: "System", icon: Shield, pages: ["status", "machines", "devices", "users", "usage", "running", "model", "activity", "settings"] },
  { id: "help", label: "Help", icon: CircleHelp, pages: ["qa", "feedback", "whatsnew"] },
];

/** The rail's pages: scrolls when they don't all fit, with a fade and an
 *  arrow at the edge that has more (and how many pages, and anything
 *  waiting, are down there). */
function RailScroll({ children }: { children: ReactNode }) {
  const ref = useRef<HTMLDivElement>(null);
  const [edges, setEdges] = useState({ up: false, down: 0, waiting: 0 });
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const look = () => {
      const bottom = el.scrollTop + el.clientHeight;
      const below = [...el.querySelectorAll<HTMLElement>("button[aria-label]")].filter((b) => b.offsetTop + b.offsetHeight / 2 > bottom);
      const waiting = below.reduce((n, b) => n + (Number(b.querySelector("[data-badge]")?.textContent) || 0), 0);
      setEdges({ up: el.scrollTop > 4, down: below.length, waiting });
    };
    look();
    el.addEventListener("scroll", look, { passive: true });
    const seen = new ResizeObserver(look);
    seen.observe(el);
    return () => {
      el.removeEventListener("scroll", look);
      seen.disconnect();
    };
  }, []);
  const page = (dir: 1 | -1) => ref.current?.scrollBy({ top: dir * (ref.current.clientHeight - 48), behavior: "smooth" });
  return (
    <div className="relative flex min-h-0 w-full flex-1 flex-col">
      <div ref={ref} className="relative flex min-h-0 w-full flex-1 flex-col items-center gap-1 overflow-x-hidden overflow-y-auto pt-0.5 [scrollbar-width:none] [&::-webkit-scrollbar]:hidden">
        {children}
      </div>
      {edges.up && (
        <button type="button" aria-label="More pages above" onClick={() => page(-1)} className="absolute inset-x-0 top-0 flex h-7 items-start justify-center bg-gradient-to-b from-sidebar to-transparent text-sidebar-foreground/70">
          <ChevronUp className="size-4" />
        </button>
      )}
      {edges.down > 0 && (
        <button
          type="button"
          aria-label={`${edges.down} more pages below`}
          onClick={() => page(1)}
          className="absolute inset-x-0 bottom-0 flex h-9 flex-col items-center justify-end bg-gradient-to-t from-sidebar via-sidebar/90 to-transparent pb-0.5 text-sidebar-foreground/80"
        >
          <ChevronDown className="size-4" />
          <span className={cn("text-[10px] leading-none", edges.waiting > 0 && "font-semibold text-amber-400")}>{edges.waiting > 0 ? `${edges.waiting} waiting` : `${edges.down} more`}</span>
        </button>
      )}
    </div>
  );
}

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
  // Each group with the pages this person has.
  const groups = railGroups.map((g) => ({ ...g, items: g.pages.map((id) => all.find((t) => t.id === id)).filter((t): t is TabItem => !!t) })).filter((g) => g.items.length > 0);
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
          // A short screen (a phone, a laptop): tighter, so more of them show.
          "[@media(max-height:820px)]:gap-0 [@media(max-height:820px)]:py-0.5 [@media(max-height:820px)]:[&>svg]:size-4",
          tab === t.id && "bg-primary/15 text-primary hover:bg-primary/20 hover:text-primary",
        )}
      >
        <t.icon />
        <span className="w-full truncate text-center text-[10px] leading-tight">{t.label}</span>
        {!!t.badge && <span data-badge className="absolute top-0.5 right-2 min-w-4 rounded-full bg-amber-400 px-1 text-center font-semibold text-[10px] text-black leading-4">{t.badge}</span>}
      </button>,
      t.id,
    );
  // A group: its icon (lit while one of its pages is open, with what waits in
  // it), and a menu of its pages beside the rail.
  // A group folds open under its icon (one at a time); the one holding the
  // open page opens by itself. Folded, it shows what waits inside it.
  const holding = railGroups.find((g) => g.pages.includes(tab))?.id ?? null;
  const [unfolded, setUnfolded] = useState<string | null>(holding);
  useEffect(() => setUnfolded(holding), [holding]);
  // Groups this person keeps open on this device (Profile → Layout, or the pin).
  const [pinned, setPinned] = useState<string[]>(loadOpen);
  useEffect(() => onOpenChange(setPinned), []);
  const railGroup = (label: string, Icon: typeof MessageSquare, items: TabItem[], key: string) => {
    const kept = pinned.includes(key);
    const open = kept || unfolded === key;
    const here = items.some((t) => t.id === tab);
    const waiting = items.reduce((n, t) => n + (t.badge ?? 0), 0);
    return (
      <div key={key} className={cn("flex w-full flex-col items-center rounded-md", open && "bg-sidebar-accent/40 pb-0.5")}>
        <button
          type="button"
          aria-label={label}
          aria-expanded={open}
          title={kept ? `${label} stays open (Profile → Layout)` : undefined}
          onClick={() => !kept && setUnfolded(open ? null : key)}
          className={cn(
            "relative flex w-14 shrink-0 flex-col items-center justify-center gap-0.5 rounded-md py-1 text-sidebar-foreground/70 transition-colors hover:bg-sidebar-accent hover:text-sidebar-accent-foreground [&>svg]:size-[18px]",
            here && !open && "bg-primary/15 text-primary hover:bg-primary/20 hover:text-primary",
            open && "text-sidebar-foreground",
          )}
        >
          <Icon />
          <span className="w-full truncate text-center text-[10px] leading-tight">{label}</span>
          {kept ? <Pin className="!size-2.5 absolute top-2 left-0.5 opacity-60" /> : <ChevronDown className={cn("!size-2.5 absolute top-2 left-0.5 opacity-60 transition-transform", open && "rotate-180")} />}
          {waiting > 0 && !open && <span data-badge className="absolute top-0.5 right-2 min-w-4 rounded-full bg-amber-400 px-1 text-center font-semibold text-[10px] text-black leading-4">{waiting}</span>}
        </button>
        {open && (
          <div className="flex flex-col items-center [&_button]:py-0.5 [&_button>svg]:size-4">
            {items.map(railItem)}
            <button
              type="button"
              aria-label={kept ? `Let ${label} fold` : `Keep ${label} open`}
              title={kept ? `Let ${label} fold again` : `Keep ${label} open`}
              onClick={() => setOpen(key, !kept)}
              className={cn("flex h-4 w-10 items-center justify-center rounded text-sidebar-foreground/40 hover:text-sidebar-foreground", kept && "text-primary/70")}
            >
              {kept ? <PinOff className="!size-3" /> : <Pin className="!size-3" />}
            </button>
          </div>
        )}
      </div>
    );
  };
  return (
    <Sidebar collapsible="offcanvas" variant="inset">
      {/* On a phone it's a sheet over the whole screen: clear of the status bar and the home bar. */}
      <div className="flex h-full min-h-0 max-md:pt-[env(safe-area-inset-top)] max-md:pb-[env(safe-area-inset-bottom)]">
        {/* The rail: lyra's pages, with what waits on each. */}
        <nav className="flex w-16 shrink-0 flex-col items-center gap-1 py-2">
          {tip(
            connected ? "lyra · connected" : "lyra · not connected",
            <button type="button" aria-label="lyra" onClick={() => go("chat")} className="relative mb-1 flex size-9 items-center justify-center">
              <img src="/icon-192.png" alt="" className="size-6 rounded" />
              <span className={cn("absolute right-0.5 bottom-0.5 size-2 rounded-full ring-2 ring-sidebar", connected ? "bg-emerald-500" : "bg-red-500")} />
            </button>,
          )}
          {/* Scrolls on a very short screen, never sideways, without a bar; says when there's more. */}
          <RailScroll>
            {groups.map((g, i) => (
              <div key={g.id} className={cn("flex flex-col items-center", (i === 2 || g.id === "help") && "mt-0.5 border-sidebar-border border-t pt-1", g.id === "help" && "mt-auto")}>
                {g.items.length === 1 ? railItem({ ...g.items[0], label: g.items[0].label }) : railGroup(g.label, g.icon, g.items, g.id)}
              </div>
            ))}
          </RailScroll>
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
              <DropdownMenuItem onClick={() => go("profile")}>
                <UserRound /> Profile
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => go("me")}>
                <Brain /> What lyra knows about me
              </DropdownMenuItem>
              {/* Settings live here, not on the rail (it's full on a laptop screen). */}
              {(user?.admin ?? true) && (
                <DropdownMenuItem onClick={() => go("settings")}>
                  <SettingsIcon /> Settings
                </DropdownMenuItem>
              )}
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
  const { status, connected, banner, serverVersion, ready, user, say } = useLyra();
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
  // Opened from a notification for a page (the briefing → Today): go there.
  useEffect(() => {
    const go = (page: string | null) => {
      // Documents are notes now.
      if (page === "documents") page = "notes";
      if (page && (["status", "machines", "devices", "activity", "more", ...manage] as string[]).includes(page)) setTab(page as Tab);
    };
    const asked = new URLSearchParams(location.search).get("page");
    if (asked) {
      history.replaceState(null, "", "/");
      go(asked);
    } else if (location.search.includes("do=")) {
      history.replaceState(null, "", "/");
    }
    const onMessage = (e: MessageEvent) => go(e.data?.page ?? null);
    navigator.serviceWorker?.addEventListener("message", onMessage);
    return () => navigator.serviceWorker?.removeEventListener("message", onMessage);
  }, []);
  // The New chat shortcut: a new conversation once lyra is there.
  useEffect(() => {
    if (ready && takeIntent("new")) {
      say("/new");
      setTab("chat");
    }
  }, [ready, say]);
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
    { id: "today", label: "Today", icon: Sun },
    { id: "agents", label: "Agents", icon: Bot },
    { id: "tasks", label: "Tasks", icon: ListTodo, badge: (status.pmi?.tasks ?? []).filter((t) => t.due && t.due < todayKey).length + pmiWaiting },
    { id: "meetings", label: "Meetings", icon: CalendarDays },
    { id: "notes", label: "Notes", icon: NotebookPen },
    { id: "whatsnew", label: "What's new", icon: Sparkles },
    { id: "feedback", label: "Feedback", icon: MessageSquareWarning, badge: status.feedback_news ?? 0 },
    { id: "qa", label: "Q&A", icon: CircleHelp },
    { id: "projects", label: "Projects", icon: FolderOpen },
    { id: "routines", label: "Routines", icon: AlarmClock, badge: (status.routines ?? []).filter((r) => r.runs[0]?.needs_user).length },
    { id: "coding", label: "Coding", icon: Code2 },
    { id: "memory", label: "Memory", icon: Brain },
    { id: "skills", label: "Skills", icon: GraduationCap },
    { id: "goals", label: "Goals", icon: Target },
    { id: "model", label: "Model", icon: Cpu },
    { id: "usage", label: "Usage", icon: Gauge },
    { id: "running", label: "Running now", icon: Timer },
    { id: "users", label: "Users", icon: Users, badge: status.users_waiting ?? 0 },
    { id: "settings", label: "Settings", icon: SettingsIcon },
    { id: "profile", label: "Profile", icon: UserRound },
    { id: "me", label: "About me", icon: Brain },
  ] as TabItem[]).filter((t) => admin || forMembers.includes(t.id));
  const toMore = () => setTab("more");
  const tabs: TabItem[] = ([
    { id: "chat", label: "Chat", icon: MessageSquare, badge: asking ? 1 : 0 },
    { id: "status", label: "Status", icon: Activity, badge: (status.status?.rows ?? []).filter((r) => r.state === "down" && r.group !== "Machines" && !status.status?.known_down?.[r.id]).length },
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
      className="h-(--app-height,100dvh) min-h-0 bg-sidebar text-foreground"
      style={{ "--sidebar-width": "calc(var(--spacing) * 88)", "--header-height": "calc(var(--spacing) * 12)" } as React.CSSProperties}
    >
      <AppSidebar tabs={tabs} more={more} tab={tab} setTab={setTab} update={updateApp} />
      <SidebarInset className="min-h-0 min-w-0 overflow-hidden pb-[env(safe-area-inset-bottom)] md:pb-0 [html[data-keyboard]_&]:pb-0">
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
          <Suspense fallback={<Loading />}>
          {tab === "chat" && <ChatPage />}
          {tab === "status" && admin && <StatusPage toMachines={() => setTab("machines")} toChat={() => setTab("chat")} />}
          {tab === "today" && <TodayPage go={(p) => setTab(p as Tab)} />}
          {tab === "agents" && admin && <AgentsPage onBack={toMore} />}
          {tab === "machines" && <MachinesPage mention={mention} toStatus={() => setTab("status")} toChat={() => setTab("chat")} />}
          {tab === "devices" && <DevicesPage />}
          {tab === "activity" && <ActivityPage />}
          {tab === "more" && <MorePage toChat={() => setTab("chat")} open={setTab} update={updateApp} />}
          {tab === "tasks" && <TasksPage onBack={toMore} />}
          {tab === "notes" && <NotesPage onBack={toMore} />}
          {tab === "whatsnew" && <WhatsNewPage onBack={toMore} />}
          {tab === "feedback" && <FeedbackPage onBack={toMore} />}
          {tab === "qa" && <QAPage onBack={toMore} />}
          {tab === "projects" && <ProjectsPage onBack={toMore} />}
          {tab === "routines" && <RoutinesPage onBack={toMore} toChat={() => setTab("chat")} />}
          {tab === "coding" && <CodingPage onBack={toMore} />}
          {tab === "memory" && <MemoryPage onBack={toMore} />}
          {tab === "skills" && <SkillsPage onBack={toMore} />}
          {tab === "goals" && <GoalsPage onBack={toMore} />}
          {tab === "model" && <ModelsPage onBack={toMore} />}
          {tab === "usage" && <UsagePage onBack={toMore} />}
          {tab === "running" && <RunningPage onBack={toMore} />}
          {tab === "users" && <UsersPage onBack={toMore} />}
          {tab === "settings" && <SettingsPage onBack={toMore} />}
          {tab === "meetings" && <MeetingsPage onBack={toMore} />}
          {tab === "me" && <AboutMePage onBack={toMore} onProfile={() => setTab("profile")} />}
          {tab === "profile" && <ProfilePage onBack={toMore} toAboutMe={() => setTab("me")} />}
          </Suspense>
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
  // How this server lets people in: a username and password (no Microsoft
  // needed), Microsoft sign-in when it's set up, and a pairing code always.
  const [ways, setWays] = useState<{ entra: boolean; passwords: boolean }>({ entra: false, passwords: false });
  // A reset link from the email: choose the new password here.
  const [reset] = useState(() => new URLSearchParams(location.search).get("reset") ?? "");
  const [mode, setMode] = useState<"password" | "microsoft" | "code" | "forgot" | "reset" | null>(() => (reset ? "reset" : new URLSearchParams(location.search).has("pair") ? "code" : null));
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [told, setTold] = useState("");
  useEffect(() => {
    if (reset) history.replaceState(null, "", "/");
  }, [reset]);
  useEffect(() => {
    void fetch("/api/auth")
      .then((r) => r.json())
      .then((b) => {
        const w = { entra: !!b.entra, passwords: !!b.passwords };
        setWays(w);
        setMode((m) => m ?? (w.passwords ? "password" : w.entra ? "microsoft" : "code"));
      })
      .catch(() => setMode((m) => m ?? "code"));
  }, []);
  const microsoft = () => {
    location.href = `/auth/login?device=${encodeURIComponent(name)}`;
  };
  const send = async (url: string, body: object) => {
    setBusy(true);
    setError("");
    const r = await fetch(url, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) }).catch(() => null);
    const b = r ? await r.json().catch(() => ({})) : { error: "Couldn't reach lyra." };
    setBusy(false);
    if (!r?.ok || !b.token) {
      setError(b.error || "That didn't work");
      return;
    }
    saveToken(b.token);
    onPaired(b.token);
  };
  const pair = (e: FormEvent) => {
    e.preventDefault();
    void send("/api/pair", { code, name });
  };
  const signIn = (e: FormEvent) => {
    e.preventDefault();
    void send("/api/auth/password", { username, password, device: name });
  };
  const forgot = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError("");
    const r = await fetch("/api/auth/password/forgot", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ username }) }).catch(() => null);
    const b = r ? await r.json().catch(() => ({})) : { error: "Couldn't reach lyra." };
    setBusy(false);
    if (r?.ok) setTold(b.text ?? "Check your email.");
    else setError(b.error || "That didn't work");
  };
  const choose = (e: FormEvent) => {
    e.preventDefault();
    if (password !== again) return setError("Those two don't match.");
    void send("/api/auth/password/reset", { token: reset, new: password, device: name });
  };
  const other = (label: string, to: "password" | "microsoft" | "code") => (
    <button key={to} type="button" onClick={() => (to === "microsoft" ? microsoft() : (setMode(to), setError("")))} className="w-full text-center text-muted-foreground text-sm hover:text-foreground">
      {label}
    </button>
  );
  const others = [
    mode !== "password" && (ways.passwords || mode === "forgot" || mode === "reset") && other(mode === "forgot" || mode === "reset" ? "Back to signing in" : "Sign in with a username instead", "password"),
    mode !== "microsoft" && ways.entra && other("Sign in with Microsoft instead", "microsoft"),
    mode !== "code" && other("Pair with a code instead", "code"),
  ].filter(Boolean);
  return (
    // The page itself doesn't scroll (index.css): this screen does, centred while it fits.
    <div className="flex h-full flex-col overflow-y-auto bg-background p-5 pt-[max(1.25rem,env(safe-area-inset-top))] pb-[max(1.25rem,env(safe-area-inset-bottom))] text-foreground">
      <Card className="m-auto w-full max-w-sm">
        <CardHeader className="items-center text-center">
          <img src="/icon-192.png" alt="" className="mx-auto mb-2 size-16 rounded-2xl" />
          <CardTitle className="text-xl">{mode === "code" ? "Pair this device" : mode === "forgot" ? "Forgot your password?" : mode === "reset" ? "Choose a new password" : "Sign in to lyra"}</CardTitle>
          <CardDescription>
            {mode === "forgot" ? (
              "Type your username: lyra emails a link to the address on your account."
            ) : mode === "reset" ? (
              "At least 10 characters (a few words together is easy to remember). Then you're signed in."
            ) : mode === "password" ? (
              "With the username and password your admin gave you."
            ) : mode === "microsoft" ? (
              "Use your organization's Microsoft account."
            ) : (
              <>
                On the computer running lyra, run <code className="rounded bg-muted px-1">lyra pair</code> (or <code className="rounded bg-muted px-1">lyra-pair</code> on the server) and enter the code it
                shows, or scan its QR code.
              </>
            )}
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-3">
          {mode === "password" && (
            <form onSubmit={signIn} className="space-y-3">
              <Input value={username} onChange={(e) => setUsername(e.target.value)} placeholder="Username" autoCapitalize="none" autoCorrect="off" spellCheck={false} autoComplete="username" className="h-11" required aria-label="Username" />
              <Input value={password} onChange={(e) => setPassword(e.target.value)} type="password" placeholder="Password" autoComplete="current-password" className="h-11" required aria-label="Password" />
              <Button type="submit" disabled={busy} className="h-11 w-full">
                {busy ? "Signing in…" : "Sign in"}
              </Button>
              <button type="button" onClick={() => (setMode("forgot"), setError(""), setTold(""))} className="w-full text-center text-muted-foreground text-xs hover:text-foreground">
                Forgot your password?
              </button>
            </form>
          )}
          {mode === "forgot" &&
            (told ? (
              <p className="text-center text-sm text-teal-300">{told}</p>
            ) : (
              <form onSubmit={(e) => void forgot(e)} className="space-y-3">
                <Input value={username} onChange={(e) => setUsername(e.target.value)} placeholder="Username" autoCapitalize="none" autoCorrect="off" spellCheck={false} autoComplete="username" className="h-11" required aria-label="Username" />
                <Button type="submit" disabled={busy} className="h-11 w-full">
                  {busy ? "Sending…" : "Email me a link"}
                </Button>
              </form>
            ))}
          {mode === "reset" && (
            <form onSubmit={choose} className="space-y-3">
              <Input value={password} onChange={(e) => setPassword(e.target.value)} type="password" placeholder="New password" autoComplete="new-password" className="h-11" required minLength={10} aria-label="New password" />
              <Input value={again} onChange={(e) => setAgain(e.target.value)} type="password" placeholder="Once more" autoComplete="new-password" className="h-11" required aria-label="New password again" />
              <Button type="submit" disabled={busy} className="h-11 w-full">
                Save and sign in
              </Button>
            </form>
          )}
          {mode === "microsoft" && (
            <Button onClick={microsoft} className="h-11 w-full">
              Sign in with Microsoft
            </Button>
          )}
          {mode === "code" && (
            <form onSubmit={pair} className="space-y-3">
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
            </form>
          )}
          {error && <p className="text-center text-red-400 text-sm">{error}</p>}
          {mode && others}
        </CardContent>
      </Card>
    </div>
  );
}

/** Signed in with a one-time password (a new account, or a reset one): choose your own first. */
function ChoosePassword({ token, onDone }: { token: string; onDone: () => void }) {
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState(false);
  const save = async (e: FormEvent) => {
    e.preventDefault();
    if (password !== again) return setError("Those two don't match.");
    setBusy(true);
    const r = await fetch("/api/auth/password/change", { method: "POST", headers: { "Content-Type": "application/json", Authorization: "Bearer " + token }, body: JSON.stringify({ new: password }) }).catch(() => null);
    const b = r ? await r.json().catch(() => ({})) : { error: "Couldn't reach lyra." };
    setBusy(false);
    if (r?.ok) onDone();
    else setError(b.error || "That didn't work");
  };
  return (
    <div className="flex h-full flex-col overflow-y-auto bg-background p-5 pt-[max(1.25rem,env(safe-area-inset-top))] pb-[max(1.25rem,env(safe-area-inset-bottom))] text-foreground">
      <Card className="m-auto w-full max-w-sm">
        <CardHeader className="items-center text-center">
          <img src="/icon-192.png" alt="" className="mx-auto mb-2 size-16 rounded-2xl" />
          <CardTitle className="text-xl">Choose your password</CardTitle>
          <CardDescription>The one you were given works once. Pick your own: at least 10 characters (a few words together is easy to remember).</CardDescription>
        </CardHeader>
        <CardContent>
          <form onSubmit={(e) => void save(e)} className="space-y-3">
            <Input value={password} onChange={(e) => setPassword(e.target.value)} type="password" placeholder="New password" autoComplete="new-password" className="h-11" required minLength={10} aria-label="New password" />
            <Input value={again} onChange={(e) => setAgain(e.target.value)} type="password" placeholder="Once more" autoComplete="new-password" className="h-11" required aria-label="New password again" />
            <Button type="submit" disabled={busy} className="h-11 w-full">
              Save and continue
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
    <div className="flex h-full flex-col overflow-y-auto bg-background p-5 pt-[max(1.25rem,env(safe-area-inset-top))] pb-[max(1.25rem,env(safe-area-inset-bottom))] text-foreground">
      <Card className="m-auto w-full max-w-sm text-center">
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
  const [resetting, setResetting] = useState(() => new URLSearchParams(location.search).has("reset"));
  const [why, setWhy] = useState<string | undefined>(signIn.error);
  const [waiting, setWaiting] = useState(false);
  const [finishing, setFinishing] = useState(!!signIn.code);
  // Signed in with a one-time password: they choose their own before anything else.
  const [mustChange, setMustChange] = useState(false);
  useEffect(() => {
    if (!token) return;
    void fetch("/api/me", { headers: { Authorization: "Bearer " + token } })
      .then((r) => (r.ok ? r.json() : null))
      .then((b) => setMustChange(!!b?.must_change))
      .catch(() => {});
  }, [token]);
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
  if (finishing) return <div className="flex h-full items-center justify-center bg-background text-muted-foreground text-sm">Signing you in…</div>;
  // A reset link opens the sign-in page even on a browser already signed in.
  if (!token || resetting) return <Pair onPaired={(t) => (setResetting(false), setToken(t))} message={why} />;
  if (mustChange) return <ChoosePassword token={token} onDone={() => setMustChange(false)} />;
  return (
    <LyraProvider token={token} onUnpaired={unpaired}>
      <TooltipProvider>
        <Shell />
      </TooltipProvider>
    </LyraProvider>
  );
}
