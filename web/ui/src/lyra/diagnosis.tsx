// A problem's write-up: what lyra found when it looked into it (read-only),
// with Fix it (to chat, where changes are approved as usual).

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { ChevronDown, MessageSquare, Search, Wrench } from "lucide-react";
import { useState } from "react";
import { MessageResponse } from "@/components/ai-elements/message";
import { useLyra } from "./store";
import type { Diagnosis } from "./types";

/** The first line worth showing on its own (as lyra's `headline`). */
export function headline(summary: string) {
  const clean = summary.split("\n").map((raw) => raw.replaceAll("**", "").replaceAll("`", "").trim().replace(/^[#\-*>\s]+/, "").trim());
  for (const l of clean) {
    const m = l.match(/^([^:]{1,24}):(.+)$/);
    if (m && (m[1].toLowerCase().includes("summary") || m[1].toLowerCase() === "tl;dr") && m[2].trim()) return m[2].trim().slice(0, 200);
  }
  return (clean.find((l) => l && !l.endsWith(":") && l.toLowerCase() !== "summary") ?? "").slice(0, 200);
}

/** The write-up for `problem` on `machine` (or a lyra check's key). */
export function useDiagnosis(machine: string, problem: string, key?: string): Diagnosis | undefined {
  const { status } = useLyra();
  return (status.diagnoses ?? []).find((d) => !d.resolved && ((!!key && d.key === key) || (d.machine.toLowerCase() === machine.toLowerCase() && d.problem === problem)));
}

export function DiagnosisNote({ machine, problem, diagnosisKey, toChat }: { machine: string; problem: string; diagnosisKey?: string; toChat: () => void }) {
  const { say, run } = useLyra();
  const d = useDiagnosis(machine, problem, diagnosisKey);
  const [open, setOpen] = useState(false);
  const [asked, setAsked] = useState(false);
  if (!d || d.state === "failed") {
    return (
      <button
        type="button"
        disabled={asked}
        onClick={async () => {
          setAsked(true);
          await run(`/diagnose ${machine} ${problem}`);
        }}
        className="ml-4 inline-flex items-center gap-1 text-sky-300 text-xs hover:text-sky-200 disabled:opacity-60"
      >
        <Search className="size-3" /> {asked ? "asked…" : d ? "couldn't finish, try again" : "look into it"}
      </button>
    );
  }
  if (d.state !== "done") {
    return (
      <div className="ml-4 flex items-center gap-1.5 text-sky-300 text-xs">
        <Search className="size-3 animate-pulse" /> {d.state === "running" ? "looking into it…" : "waiting to be looked into…"}
      </div>
    );
  }
  return (
    <div className="ml-4 space-y-1.5 rounded-md border border-sky-800/50 bg-sky-950/20 px-3 py-2 text-xs">
      <button type="button" onClick={() => setOpen(!open)} className="flex w-full items-start gap-1.5 text-left text-sky-100">
        <Search className="mt-0.5 size-3 shrink-0 text-sky-300" />
        <span className="flex-1">{headline(d.summary)}</span>
        <ChevronDown className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} />
      </button>
      {open && (
        <div className="prose-sm max-w-none text-foreground/90 [&_code]:text-[11px] [&_pre]:text-[11px]">
          <MessageResponse>{d.summary}</MessageResponse>
        </div>
      )}
      <div className="flex flex-wrap gap-2">
        <Button
          size="sm"
          className="h-7 text-xs"
          onClick={() => {
            say(`Please fix this on @${machine}: ${problem}\n\nWhat you found when you looked into it:\n${d.summary}`);
            toChat();
          }}
        >
          <Wrench /> Fix it
        </Button>
        {d.session && (
          <Button
            size="sm"
            variant="ghost"
            className="h-7 text-xs"
            onClick={() => {
              say(`/resume ${d.session}`);
              toChat();
            }}
          >
            <MessageSquare /> Open conversation
          </Button>
        )}
      </div>
    </div>
  );
}
