// One search for everything (Ctrl-K / ⌘K): conversations, notes, memories,
// mail, Teams and files at once, each the person's own. Picking a result
// opens it: a conversation here, a page, or the mail, chat or file in Microsoft 365.
// Above them, things to do: a new task or conversation, Talk, a setting, a
// routine to run, a page to open, or ask lyra what was typed.

import { Command, CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command";
import { AlarmClock, ArrowRight, AudioLines, Brain, ExternalLink, FileText, ListPlus, Loader2, Mail, MessageSquare, MessageSquarePlus, NotebookPen, Settings, Sparkles, Users } from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { useLyra } from "./store";

type Hit = { title: string; detail?: string; open: { session?: string; page?: string; link?: string } };
type Found = { query: string; groups: { name: string; items: Hit[] }[] };

const ICONS: Record<string, typeof Mail> = { Conversations: MessageSquare, Notes: NotebookPen, Memories: Brain, Mail, Teams: Users, Files: FileText };

type Action = { key: string; label: string; detail?: string; icon: typeof Mail; run: () => void };

/** Every word typed is somewhere in it. */
const fits = (q: string, text: string) => q.split(/\s+/).filter(Boolean).every((w) => text.toLowerCase().includes(w));

export function EverythingSearch({ open, setOpen, go, pages = [] }: { open: boolean; setOpen: (o: boolean) => void; go: (page: string) => void; pages?: { id: string; label: string }[] }) {
  const { call, say, run, status } = useLyra();
  const [query, setQuery] = useState("");
  // What the last action did (a task added): the palette stays open for another.
  const [done, setDone] = useState<{ ok: boolean; text: string } | null>(null);
  const [found, setFound] = useState<Found | null>(null);
  const [busy, setBusy] = useState(false);
  const asked = useRef(0);
  // Asked a moment after typing stops; only the latest answer counts.
  useEffect(() => {
    const q = query.trim();
    if (q.length < 2) {
      setFound(null);
      return;
    }
    const n = ++asked.current;
    const t = window.setTimeout(() => {
      setBusy(true);
      void call<Found>("everything", { query: q }).then((f) => {
        if (n === asked.current) {
          setFound(f);
          setBusy(false);
        }
      });
    }, 350);
    return () => window.clearTimeout(t);
  }, [query, call]);
  const actions = useMemo<Action[]>(() => {
    const raw = query.trim();
    const q = raw.toLowerCase();
    const out: Action[] = [];
    const close = (f: () => void) => () => {
      setOpen(false);
      f();
    };
    const doing = (command: string) => () => {
      void run(command).then((r) => {
        setDone(r);
        if (r.ok) setQuery("");
      });
    };
    // "task call the vendor friday" → added as it's said.
    const task = raw.match(/^(?:new task|add task|add a task|task|todo)[:\s]+(.+)$/i);
    if (task) out.push({ key: "task", label: `Add task “${task[1]}”`, detail: "dates and times in it are read: “friday 3pm”", icon: ListPlus, run: doing(`/task add ${task[1]}`) });
    else if (!q || fits(q, "new task add todo")) out.push({ key: "task-start", label: "New task…", detail: "type what to do after “task”", icon: ListPlus, run: () => setQuery("task ") });
    if (!q || fits(q, "new conversation chat start")) out.push({ key: "new", label: "New conversation", icon: MessageSquarePlus, run: close(() => (say("/new"), go("chat"))) });
    if (pages.some((p) => p.id === "talk") && (!q || fits(q, "start talk voice speak hands-free"))) out.push({ key: "talk", label: "Start Talk", detail: "talk with lyra, hands-free", icon: AudioLines, run: close(() => go("talk")) });
    // "settings fallback" → Settings, Everything, searched for it.
    const setting = raw.match(/^(?:open settings|settings|setting)[:\s]*(.*)$/i);
    if (pages.some((p) => p.id === "settings") && (setting || !q)) {
      const what = setting?.[1]?.trim() ?? "";
      out.push({
        key: "settings",
        label: what ? `Settings: find “${what}”` : "Open Settings",
        icon: Settings,
        run: close(() => {
          try {
            if (what) {
              localStorage.setItem("lyra-settings-tab", "all");
              sessionStorage.setItem("lyra-settings-find", what);
            }
          } catch {
            // the page opens as it was
          }
          go("settings");
        }),
      });
    }
    // Their routines: run one now.
    if (q)
      for (const r of status.routines ?? [])
        if (fits(q.replace(/^run\s+/, ""), `run routine ${r.name}`)) out.push({ key: `routine-${r.name}`, label: `Run routine ${r.name}`, detail: r.on ? `on ${r.on}` : r.schedule, icon: AlarmClock, run: doing(`/routine run ${r.name}`) });
    // A page.
    if (q && q.length >= 2)
      for (const p of pages) if (fits(q.replace(/^(go to|open)\s+/, ""), p.label)) out.push({ key: `page-${p.id}`, label: `Go to ${p.label}`, icon: ArrowRight, run: close(() => go(p.id)) });
    if (raw.length >= 2) out.push({ key: "ask", label: `Ask lyra: “${raw}”`, icon: Sparkles, run: close(() => (say(raw), go("chat"))) });
    return out.slice(0, 9);
  }, [query, pages, status.routines, run, say, go, setOpen]);
  const pick = (h: Hit) => {
    setOpen(false);
    if (h.open.session) {
      say(`/resume ${h.open.session}`);
      go("chat");
    } else if (h.open.page) go(h.open.page);
    else if (h.open.link) window.open(h.open.link, "_blank", "noopener");
  };
  return (
    <CommandDialog open={open} onOpenChange={setOpen} title="Search everything" description="Conversations, notes, memories, mail, Teams and files" className="sm:max-w-2xl">
      {/* lyra searches; the list shows what it found as it is. */}
      <Command shouldFilter={false}>
        <CommandInput value={query} onValueChange={(v) => (setQuery(v), setDone(null))} placeholder="Search, or do: “task call the vendor friday”, “settings fallback”, “run …”" />
        <CommandList className="max-h-[60vh]">
          {done && <div className={done.ok ? "px-3 py-2 text-teal-300 text-xs" : "px-3 py-2 text-red-300 text-xs"}>{done.text}</div>}
          {actions.length > 0 && (
            <CommandGroup heading="Do">
              {actions.map((a) => (
                <CommandItem key={a.key} value={`do-${a.key}`} onSelect={a.run} className="items-start">
                  <a.icon className="mt-0.5 size-4 shrink-0 text-amber-300" />
                  <span className="grid min-w-0 flex-1">
                    <span className="truncate">{a.label}</span>
                    {a.detail && <span className="truncate text-muted-foreground text-xs">{a.detail}</span>}
                  </span>
                </CommandItem>
              ))}
            </CommandGroup>
          )}
          {busy && (
            <div className="flex items-center gap-2 px-3 py-2 text-muted-foreground text-xs">
              <Loader2 className="size-3.5 animate-spin" /> Searching…
            </div>
          )}
          <CommandEmpty>{query.trim().length < 2 ? "Type at least two letters." : busy ? "" : "Nothing found."}</CommandEmpty>
          {found?.groups.map((g) => {
            const Icon = ICONS[g.name] ?? FileText;
            return (
              <CommandGroup key={g.name} heading={g.name}>
                {g.items.map((h, i) => (
                  <CommandItem key={`${g.name}-${i}`} value={`${g.name}-${i}`} onSelect={() => pick(h)} className="items-start">
                    <Icon className="mt-0.5 size-4 shrink-0 text-primary" />
                    <span className="grid min-w-0 flex-1">
                      <span className="truncate">{h.title}</span>
                      {h.detail && <span className="truncate text-muted-foreground text-xs">{h.detail}</span>}
                    </span>
                    {h.open.link && <ExternalLink className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />}
                  </CommandItem>
                ))}
              </CommandGroup>
            );
          })}
        </CommandList>
      </Command>
    </CommandDialog>
  );
}
