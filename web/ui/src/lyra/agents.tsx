// Agents: the specialists lyra hands work to, for admins. Each has its own
// colour and icon (as on its cards in the chat), can be shared with members
// or kept to admins (the Operator and the Coder always are), duplicated as a
// starting point for a new one, and turned off. Changes go through /agent.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Archive, BookOpen, Bot, Briefcase, CalendarDays, ChartLine, Code2, Copy, PenLine, Search, Server, Shield, Sparkles, type LucideIcon } from "lucide-react";
import { useState } from "react";
import { Back, Page, useAction } from "./parts";
import { useLyra } from "./store";

export const AGENT_ICONS: Record<string, LucideIcon> = {
  bot: Bot,
  pen: PenLine,
  search: Search,
  archive: Archive,
  server: Server,
  code: Code2,
  briefcase: Briefcase,
  calendar: CalendarDays,
  chart: ChartLine,
  shield: Shield,
  book: BookOpen,
  sparkles: Sparkles,
};

/** Each colour's classes (written out, so the styles are built in). */
export const AGENT_COLORS: Record<string, { card: string; icon: string; text: string; dot: string }> = {
  sky: { card: "border-sky-900/60 bg-sky-950/30", icon: "text-sky-400", text: "text-sky-200", dot: "bg-sky-500" },
  teal: { card: "border-teal-900/60 bg-teal-950/30", icon: "text-teal-400", text: "text-teal-200", dot: "bg-teal-500" },
  emerald: { card: "border-emerald-900/60 bg-emerald-950/30", icon: "text-emerald-400", text: "text-emerald-200", dot: "bg-emerald-500" },
  amber: { card: "border-amber-900/60 bg-amber-950/30", icon: "text-amber-400", text: "text-amber-200", dot: "bg-amber-500" },
  orange: { card: "border-orange-900/60 bg-orange-950/30", icon: "text-orange-400", text: "text-orange-200", dot: "bg-orange-500" },
  rose: { card: "border-rose-900/60 bg-rose-950/30", icon: "text-rose-400", text: "text-rose-200", dot: "bg-rose-500" },
  violet: { card: "border-violet-900/60 bg-violet-950/30", icon: "text-violet-400", text: "text-violet-200", dot: "bg-violet-500" },
  fuchsia: { card: "border-fuchsia-900/60 bg-fuchsia-950/30", icon: "text-fuchsia-400", text: "text-fuchsia-200", dot: "bg-fuchsia-500" },
  slate: { card: "border-slate-700/60 bg-slate-900/40", icon: "text-slate-300", text: "text-slate-200", dot: "bg-slate-400" },
};

export interface AgentRow {
  name: string;
  title: string;
  description: string;
  working: boolean;
  enabled: boolean;
  color: string;
  icon: string;
  shared: boolean;
  admin_only: boolean;
  delegations: number;
}

/** On/off, as a switch (like the Settings page's). */
function Switch({ checked, onCheckedChange, disabled, ...rest }: { checked: boolean; onCheckedChange: (on: boolean) => void; disabled?: boolean; "aria-label": string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={rest["aria-label"]}
      disabled={disabled}
      onClick={() => onCheckedChange(!checked)}
      className={cn("relative inline-flex h-6 w-11 shrink-0 items-center rounded-full border transition-colors disabled:opacity-50", checked ? "border-teal-600 bg-teal-600" : "border-muted-foreground/40 bg-muted")}
    >
      <span className={cn("inline-block size-4 rounded-full shadow transition-transform", checked ? "translate-x-6 bg-white" : "translate-x-1 bg-muted-foreground")} />
    </button>
  );
}

/** How an agent looks: its colour's classes and its icon. */
export function agentLook(a?: { color?: string; icon?: string }) {
  return { colors: AGENT_COLORS[a?.color ?? ""] ?? AGENT_COLORS.sky, Icon: AGENT_ICONS[a?.icon ?? ""] ?? Bot };
}

function AgentCard({ a, act, busy }: { a: AgentRow; act: (c: string) => Promise<boolean>; busy: boolean }) {
  const { colors, Icon } = agentLook(a);
  const [copy, setCopy] = useState<string | null>(null);
  return (
    <Card className={cn("gap-2 py-3", colors.card, !a.enabled && "opacity-60")}>
      <CardHeader className="px-4">
        <CardTitle className={cn("flex items-center gap-2 text-sm", colors.text)}>
          <Icon className={cn("size-4", colors.icon)} /> {a.title}
          {!a.enabled && <Badge variant="outline">off</Badge>}
          {a.working && <Badge variant="secondary">working</Badge>}
        </CardTitle>
        <CardDescription>{a.description}</CardDescription>
        <CardAction className="text-muted-foreground text-xs">{a.delegations ? `${a.delegations}×` : ""}</CardAction>
      </CardHeader>
      <CardContent className="space-y-3 px-4">
        <div className="flex flex-wrap items-center gap-1.5" aria-label="Colour">
          {Object.entries(AGENT_COLORS).map(([c, cls]) => (
            <button
              key={c}
              type="button"
              title={c}
              aria-label={`Colour ${c}`}
              aria-pressed={a.color === c}
              disabled={busy}
              onClick={() => void act(`/agent look ${a.name} ${c}`)}
              className={cn("size-5 rounded-full ring-offset-2 ring-offset-background", cls.dot, a.color === c && "ring-2 ring-foreground")}
            />
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-1" aria-label="Icon">
          {Object.entries(AGENT_ICONS).map(([name, I]) => (
            <Button key={name} size="icon" variant={a.icon === name ? "secondary" : "ghost"} className="size-7" title={name} aria-label={`Icon ${name}`} disabled={busy} onClick={() => void act(`/agent look ${a.name} - ${name}`)}>
              <I className="size-4" />
            </Button>
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-sm">
          <label className="flex items-center gap-2" title={a.admin_only ? "Works on machines and code: always admins' only" : "Members may use it in their conversations"}>
            <Switch checked={a.shared} disabled={busy || a.admin_only} onCheckedChange={(on) => void act(`/agent share ${a.name} ${on ? "on" : "off"}`)} aria-label="Shared with members" />
            Members may use it
          </label>
          <label className="flex items-center gap-2">
            <Switch checked={a.enabled} disabled={busy} onCheckedChange={(on) => void act(`/agent ${on ? "enable" : "disable"} ${a.name}`)} aria-label="On" />
            On
          </label>
          {copy === null ? (
            <Button size="sm" variant="ghost" className="ml-auto" onClick={() => setCopy(`${a.title} copy`)}>
              <Copy /> Duplicate
            </Button>
          ) : (
            <form
              className="ml-auto flex items-center gap-1"
              onSubmit={async (e) => {
                e.preventDefault();
                if (copy.trim() && (await act(`/agent duplicate ${a.name} ${copy.trim()}`))) setCopy(null);
              }}
            >
              <Input autoFocus value={copy} onChange={(e) => setCopy(e.target.value)} className="h-8 w-44" aria-label="The copy's name" />
              <Button size="sm" type="submit" disabled={busy || !copy.trim()}>
                Make copy
              </Button>
              <Button size="sm" variant="ghost" type="button" onClick={() => setCopy(null)}>
                Cancel
              </Button>
            </form>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

export function AgentsPage({ onBack }: { onBack: () => void }) {
  const { status } = useLyra();
  const { act, busy, note } = useAction(() => {});
  const agents = (status.agents ?? []) as AgentRow[];
  return (
    <Page title="Agents" description="The specialists lyra hands work to: how each looks, whether members may use it, and copies to start new ones from. /agent new makes one from scratch." action={<Back onBack={onBack} />}>
      {note}
      {agents.length === 0 && <p className="text-muted-foreground text-sm">No agents yet: /agent new in the chat makes one.</p>}
      <div className="grid gap-3 md:grid-cols-2">
        {agents.map((a) => (
          <AgentCard key={a.name} a={a} act={act} busy={busy} />
        ))}
      </div>
    </Page>
  );
}
