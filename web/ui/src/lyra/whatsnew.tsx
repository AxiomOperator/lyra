// What's new: every release of lyra, newest first (CHANGELOG.json, built into
// lyra), and a note after an update that links here.

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { ChevronLeft, ChevronRight, Search, Sparkles, Wrench, X, Zap } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { Back, Page } from "./parts";
import { useLyra } from "./store";

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

interface Changelog {
  version: string;
  releases: Release[];
  page: number;
  pages: number;
  total: number;
  per: number;
  /** What it was searched for ("" for everything). */
  q?: string;
}

/** Newer / Older, and where you are. */
function Pager({ data, go }: { data: Changelog; go: (page: number) => void }) {
  if (data.pages <= 1) return data.q && data.total ? <p className="text-center text-muted-foreground text-sm">{data.total === 1 ? "1 release matches" : `${data.total} releases match`}</p> : null;
  const first = data.page * data.per + 1;
  const last = Math.min(data.total, first + data.releases.length - 1);
  return (
    <div className="flex items-center justify-between gap-2 text-muted-foreground text-sm">
      <Button size="sm" variant="ghost" disabled={data.page === 0} onClick={() => go(data.page - 1)}>
        <ChevronLeft /> Newer
      </Button>
      <span>
        {first}–{last} of {data.total} {data.q ? "matching" : "releases"}
      </span>
      <Button size="sm" variant="ghost" disabled={data.page >= data.pages - 1} onClick={() => go(data.page + 1)}>
        Older <ChevronRight />
      </Button>
    </div>
  );
}

export function WhatsNewPage({ onBack }: { onBack: () => void }) {
  const { call, ready } = useLyra();
  const [page, setPage] = useState(0);
  const [data, setData] = useState<Changelog | null>(null);
  const top = useRef<HTMLDivElement>(null);
  // What's typed, and what's searched (a moment after typing stops).
  const [typed, setTyped] = useState("");
  const [q, setQ] = useState("");
  useEffect(() => {
    const t = window.setTimeout(() => {
      setQ(typed.trim());
      setPage(0);
    }, 250);
    return () => window.clearTimeout(t);
  }, [typed]);
  useEffect(() => {
    // An answer for an older search is dropped.
    let current = true;
    if (ready) void call<Changelog>("changelog", { page, q }).then((d) => current && d && Array.isArray(d.releases) && setData(d));
    return () => {
      current = false;
    };
  }, [ready, call, page, q]);
  // Seen now: the update note goes away.
  useEffect(() => {
    if (data?.version) markSeen(data.version);
  }, [data?.version]);
  const go = (p: number) => {
    setPage(p);
    top.current?.scrollIntoView({ block: "start" });
  };
  return (
    <Page title="What's new" description={data ? `You're on lyra ${data.version}.` : "Every change to lyra, newest first."} action={<Back onBack={onBack} />}>
      <div ref={top} />
      <div className="relative">
        <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
        <Input value={typed} onChange={(e) => setTyped(e.target.value)} placeholder="Search releases (e.g. calendar, passwords, 0.38)" className="pr-9 pl-9" aria-label="Search releases" />
        {typed && (
          <button type="button" aria-label="Clear the search" onClick={() => setTyped("")} className="absolute top-1/2 right-3 -translate-y-1/2 text-muted-foreground hover:text-foreground">
            <X className="size-4" />
          </button>
        )}
      </div>
      {data?.q && !data.total && <p className="py-6 text-center text-muted-foreground text-sm">No release mentions “{data.q}”.</p>}
      {data && <Pager data={data} go={go} />}
      {(data?.releases ?? []).map((r) => (
        <Card key={r.version} className="gap-3 py-4">
          <CardHeader className="px-4">
            <CardDescription className="flex flex-wrap items-center gap-2">
              <Badge variant={r.version === data?.version ? "default" : "outline"} className="font-mono">
                {r.version}
              </Badge>
              <span>{day(r.date)}</span>
              {r.version === data?.version && <span className="text-primary text-xs">current</span>}
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
      {data && data.pages > 1 && data.releases.length > 3 && <Pager data={data} go={go} />}
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
