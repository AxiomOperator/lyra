// The sidebar's conversation list: pinned, the person's folders, the rest by
// date (today, yesterday, this week, this month, earlier), and archived ones
// folded away. Each row's menu pins, files and archives it (lyra's
// `/sessions` commands); lyra suggests a folder for new ones.

import { Button } from "@/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "@/components/ui/collapsible";
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuSub,
  DropdownMenuSubContent,
  DropdownMenuSubTrigger,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { SidebarGroup, SidebarGroupContent, SidebarGroupLabel, SidebarMenu, SidebarMenuAction, SidebarMenuButton, SidebarMenuItem } from "@/components/ui/sidebar";
import { Archive, ArchiveRestore, ChevronRight, Ellipsis, Folder, FolderInput, FolderMinus, FolderPlus, Pin, PinOff, Sparkles, X } from "lucide-react";
import { useState, type FormEvent, type ReactNode } from "react";
import { ago } from "./push";
import { useLyra } from "./store";
import type { Session } from "./types";

/** Which date group a conversation falls in (by its last change, local time). */
export function dateGroup(updated: string, now = new Date()): string {
  const d = new Date(updated);
  const day = (x: Date) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((day(now) - day(d)) / 86_400_000);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 7) return "This week";
  if (days < 31) return "This month";
  return "Earlier";
}
const DATES = ["Today", "Yesterday", "This week", "This month", "Earlier"];

/** A short age for a row: 5m, 3h, 2d, 4w, 6mo. */
export function age(iso: string, now = Date.now()): string {
  const m = Math.max(0, (now - new Date(iso).getTime()) / 60_000);
  if (m < 60) return `${Math.max(1, Math.round(m))}m`;
  if (m < 1440) return `${Math.round(m / 60)}h`;
  if (m < 10080) return `${Math.round(m / 1440)}d`;
  if (m < 43200) return `${Math.round(m / 10080)}w`;
  return `${Math.round(m / 43200)}mo`;
}

/** A plain small heading (dates, Pinned). */
function Label({ children }: { children: ReactNode }) {
  return <div className="px-2 pt-3 pb-1 font-medium text-[11px] text-muted-foreground uppercase tracking-wide">{children}</div>;
}

function useOpen(key: string, initial: boolean) {
  const [open, setOpen] = useState<boolean>(() => {
    try {
      const v = localStorage.getItem(`lyra-conv-${key}`);
      return v === null ? initial : v === "1";
    } catch {
      return initial;
    }
  });
  const set = (on: boolean) => {
    setOpen(on);
    try {
      localStorage.setItem(`lyra-conv-${key}`, on ? "1" : "0");
    } catch {
      // not remembered
    }
  };
  return [open, set] as const;
}

/** A foldable heading with its rows. */
function Section({ id, label, icon, count, initial, children }: { id: string; label: string; icon?: ReactNode; count?: number; initial: boolean; children: ReactNode }) {
  const [open, setOpen] = useOpen(id, initial);
  return (
    <Collapsible open={open} onOpenChange={setOpen} className="group/conv">
      <SidebarGroupLabel asChild className="h-7">
        <CollapsibleTrigger className="flex w-full items-center gap-1 hover:text-sidebar-foreground">
          <ChevronRight className="size-3.5 transition-transform group-data-[state=open]/conv:rotate-90" />
          {icon}
          <span className="truncate">{label}</span>
          {count !== undefined && <span className="ml-auto text-[10px] tabular-nums">{count}</span>}
        </CollapsibleTrigger>
      </SidebarGroupLabel>
      <CollapsibleContent>
        <SidebarMenu>{children}</SidebarMenu>
      </CollapsibleContent>
    </Collapsible>
  );
}

export function ConversationList({ sessions, active, open, reload }: { sessions: Session[]; active: boolean; open: (id: string, current: boolean) => void; reload: () => void }) {
  const { run } = useLyra();
  const [naming, setNaming] = useState<{ id: string; name: string } | null>(null);
  const keep = async (command: string) => {
    await run(command);
    reload();
  };
  const folders = [...new Set(sessions.map((s) => s.folder).filter((f): f is string => !!f))].sort((a, b) => a.localeCompare(b));
  const live = sessions.filter((s) => !s.archived);
  const pinned = live.filter((s) => s.pinned);
  const filed = (f: string) => live.filter((s) => !s.pinned && s.folder === f);
  const loose = live.filter((s) => !s.pinned && !s.folder);
  const archived = sessions.filter((s) => s.archived);

  const row = (s: Session) => (
    <SidebarMenuItem key={s.id}>
      <SidebarMenuButton isActive={s.current && active} onClick={() => open(s.id, s.current)} title={`${s.title || "(untitled)"} · ${ago(s.updated)}`} className="group/row pr-7">
        {s.answering && <span title="answering" className="size-2 shrink-0 animate-pulse rounded-full bg-sky-400" />}
        <span className="min-w-0 flex-1 truncate">{s.title || "(untitled)"}</span>
        <span className="shrink-0 text-[11px] text-muted-foreground tabular-nums group-hover/menu-item:invisible">{age(s.updated)}</span>
      </SidebarMenuButton>
      {/* lyra's suggested folder: one tap files it. */}
      {s.suggested && !s.folder && !s.archived && (
        <div className="flex items-center gap-1 pb-1 pl-2 text-[11px] text-muted-foreground">
          <Sparkles className="size-3 text-amber-400" />
          <button type="button" className="truncate hover:text-foreground" onClick={() => void keep(`/sessions folder ${s.id} ${s.suggested}`)}>
            Move to {s.suggested}?
          </button>
          <button type="button" aria-label="No" className="hover:text-foreground" onClick={() => void keep(`/sessions dismiss ${s.id}`)}>
            <X className="size-3" />
          </button>
        </div>
      )}
      <DropdownMenu>
        <DropdownMenuTrigger asChild>
          <SidebarMenuAction showOnHover aria-label="More">
            <Ellipsis />
          </SidebarMenuAction>
        </DropdownMenuTrigger>
        <DropdownMenuContent side="right" align="start" className="min-w-48">
          {!s.archived &&
            (s.pinned ? (
              <DropdownMenuItem onClick={() => void keep(`/sessions unpin ${s.id}`)}>
                <PinOff /> Unpin
              </DropdownMenuItem>
            ) : (
              <DropdownMenuItem onClick={() => void keep(`/sessions pin ${s.id}`)}>
                <Pin /> Pin
              </DropdownMenuItem>
            ))}
          <DropdownMenuSub>
            <DropdownMenuSubTrigger>
              <FolderInput /> Move to folder
            </DropdownMenuSubTrigger>
            <DropdownMenuSubContent className="min-w-44">
              {folders
                .filter((f) => f !== s.folder)
                .map((f) => (
                  <DropdownMenuItem key={f} onClick={() => void keep(`/sessions folder ${s.id} ${f}`)}>
                    <Folder /> {f}
                  </DropdownMenuItem>
                ))}
              {folders.some((f) => f !== s.folder) && <DropdownMenuSeparator />}
              <DropdownMenuItem onClick={() => setNaming({ id: s.id, name: "" })}>
                <FolderPlus /> New folder…
              </DropdownMenuItem>
              {s.folder && (
                <DropdownMenuItem onClick={() => void keep(`/sessions folder ${s.id} -`)}>
                  <FolderMinus /> Out of {s.folder}
                </DropdownMenuItem>
              )}
            </DropdownMenuSubContent>
          </DropdownMenuSub>
          <DropdownMenuSeparator />
          {s.archived ? (
            <DropdownMenuItem onClick={() => void keep(`/sessions unarchive ${s.id}`)}>
              <ArchiveRestore /> Unarchive
            </DropdownMenuItem>
          ) : (
            <DropdownMenuItem onClick={() => void keep(`/sessions archive ${s.id}`)}>
              <Archive /> Archive
            </DropdownMenuItem>
          )}
        </DropdownMenuContent>
      </DropdownMenu>
    </SidebarMenuItem>
  );

  const create = (e: FormEvent) => {
    e.preventDefault();
    if (!naming || !naming.name.trim()) return;
    void keep(`/sessions folder ${naming.id} ${naming.name.trim()}`);
    setNaming(null);
  };

  return (
    <SidebarGroup className="min-h-0 flex-1 pt-0">
      <SidebarGroupContent className="min-h-0 flex-1 overflow-y-auto">
        {pinned.length > 0 && (
          <>
            <Label>Pinned</Label>
            <SidebarMenu>{pinned.map(row)}</SidebarMenu>
          </>
        )}
        {folders.some((f) => filed(f).length > 0) && <Label>Folders</Label>}
        {folders.map((f) =>
          filed(f).length > 0 ? (
            <Section key={f} id={`folder-${f}`} label={f} icon={<Folder className="size-3" />} count={filed(f).length} initial={false}>
              {filed(f).map(row)}
            </Section>
          ) : null,
        )}
        {DATES.map((d) => {
          const these = loose.filter((s) => dateGroup(s.updated) === d);
          return these.length > 0 ? (
            <div key={d}>
              <Label>{d}</Label>
              <SidebarMenu>{these.map(row)}</SidebarMenu>
            </div>
          ) : null;
        })}
        {archived.length > 0 && (
          <Section id="archived" label="Archived" icon={<Archive className="size-3" />} count={archived.length} initial={false}>
            {archived.map(row)}
          </Section>
        )}
      </SidebarGroupContent>
      <Dialog open={!!naming} onOpenChange={(o) => !o && setNaming(null)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New folder</DialogTitle>
          </DialogHeader>
          <form onSubmit={create} className="grid gap-3">
            <Input autoFocus maxLength={40} placeholder="e.g. Agent Portal" value={naming?.name ?? ""} onChange={(e) => setNaming((n) => n && { ...n, name: e.target.value })} />
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => setNaming(null)}>
                Cancel
              </Button>
              <Button type="submit" disabled={!naming?.name.trim()}>
                Create and move
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </SidebarGroup>
  );
}
