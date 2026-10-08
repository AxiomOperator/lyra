// What's new: every release of lyra, newest first (CHANGELOG.json, built into
// lyra), and a note after an update that links here.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Sparkles, Wrench, X, Zap } from "lucide-react";
import { useEffect, useState } from "react";
import { Back } from "./manage";
import { Page } from "./parts";
import { useData, useLyra } from "./store";

interface Release {
  version: string;
  date: string;
  title: string;
  new?: string[];
  improved?: string[];
  fixed?: string[];
}

const KINDS = [
  { key: "new", label: "New", icon: Sparkles, cls: "text-primary" },
  { key: "improved", label: "Improved", icon: Zap, cls: "text-sky-400" },
  { key: "fixed", label: "Fixed", icon: Wrench, cls: "text-amber-400" },
] as const;

function day(d: string) {
  return new Date(`${d}T12:00:00`).toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
}

export function WhatsNewPage({ onBack }: { onBack: () => void }) {
  const [data] = useData<{ version: string; releases: Release[] } | null>("changelog");
  // Seen now: the update note goes away.
  useEffect(() => {
    if (data?.version) markSeen(data.version);
  }, [data?.version]);
  return (
    <Page title="What's new" description={data ? `You're on lyra ${data.version}.` : "Every change to lyra, newest first."} action={<Back onBack={onBack} />}>
      {(data?.releases ?? []).map((r, i) => (
        <Card key={r.version} className="gap-3 py-4">
          <CardHeader className="px-4">
            <CardDescription className="flex flex-wrap items-center gap-2">
              <Badge variant={i === 0 ? "default" : "outline"} className="font-mono">
                {r.version}
              </Badge>
              <span>{day(r.date)}</span>
              {i === 0 && <span className="text-primary text-xs">current</span>}
            </CardDescription>
            <CardTitle className="text-base">{r.title}</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3 px-4">
            {KINDS.map(({ key, label, icon: Icon, cls }) =>
              r[key]?.length ? (
                <div key={key} className="space-y-1">
                  <div className={`flex items-center gap-1.5 font-medium text-xs uppercase tracking-wide ${cls}`}>
                    <Icon className="size-3.5" /> {label}
                  </div>
                  <ul className="list-disc space-y-1 pl-5 text-sm">
                    {r[key]!.map((t, j) => (
                      <li key={j}>{t}</li>
                    ))}
                  </ul>
                </div>
              ) : null,
            )}
          </CardContent>
        </Card>
      ))}
    </Page>
  );
}

const SEEN = "lyra-seen-version";

function markSeen(v: string) {
  try {
    localStorage.setItem(SEEN, v);
  } catch {
    // just shown again next time
  }
  // The note goes, wherever What's new was opened from.
  window.dispatchEvent(new CustomEvent("lyra-seen", { detail: v }));
}

/** After an update: "lyra updated to …", with a link here. The first visit only remembers the version. */
export function UpdatedNote({ open }: { open: () => void }) {
  const { status } = useLyra();
  const v = status.version;
  const [seen, setSeen] = useState<string | null>(() => {
    try {
      return localStorage.getItem(SEEN);
    } catch {
      return null;
    }
  });
  useEffect(() => {
    const on = (e: Event) => setSeen((e as CustomEvent<string>).detail);
    window.addEventListener("lyra-seen", on);
    return () => window.removeEventListener("lyra-seen", on);
  }, []);
  useEffect(() => {
    if (v && seen === null) {
      markSeen(v);
      setSeen(v);
    }
  }, [v, seen]);
  if (!v || seen === null || seen === v) return null;
  const close = () => {
    markSeen(v);
    setSeen(v);
  };
  return (
    <div className="mx-auto mt-2 flex w-full max-w-3xl items-center gap-2 rounded-lg border border-primary/30 bg-primary/10 px-3 py-2 text-sm">
      <Sparkles className="size-4 shrink-0 text-primary" />
      <span className="min-w-0 flex-1">
        lyra was updated to <span className="font-mono">{v}</span>.
      </span>
      <Button
        size="sm"
        variant="ghost"
        className="text-primary hover:text-primary"
        onClick={() => {
          close();
          open();
        }}
      >
        What's new
      </Button>
      <button type="button" aria-label="Dismiss" onClick={close} className="text-muted-foreground hover:text-foreground">
        <X className="size-4" />
      </button>
    </div>
  );
}
