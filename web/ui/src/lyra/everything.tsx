// One search for everything (Ctrl-K / ⌘K): conversations, notes, memories,
// mail, Teams and files at once, each the person's own. Picking a result
// opens it: a conversation here, a page, or the mail, chat or file in Microsoft 365.

import { Command, CommandDialog, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from "@/components/ui/command";
import { Brain, ExternalLink, FileText, Loader2, Mail, MessageSquare, NotebookPen, Users } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useLyra } from "./store";

type Hit = { title: string; detail?: string; open: { session?: string; page?: string; link?: string } };
type Found = { query: string; groups: { name: string; items: Hit[] }[] };

const ICONS: Record<string, typeof Mail> = { Conversations: MessageSquare, Notes: NotebookPen, Memories: Brain, Mail, Teams: Users, Files: FileText };

export function EverythingSearch({ open, setOpen, go }: { open: boolean; setOpen: (o: boolean) => void; go: (page: string) => void }) {
  const { call, say } = useLyra();
  const [query, setQuery] = useState("");
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
        <CommandInput value={query} onValueChange={setQuery} placeholder="Search conversations, notes, memories, mail, Teams, files…" />
        <CommandList className="max-h-[60vh]">
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
