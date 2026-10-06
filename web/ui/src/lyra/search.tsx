// Searching saved conversations by what was said in them (`/sessions search`).

import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Search, X } from "lucide-react";
import { useEffect, useState } from "react";
import { ago } from "./push";
import { useLyra } from "./store";

export interface SearchHit {
  id: string;
  title: string;
  updated: string;
  role: string;
  snippet: string;
  current: boolean;
}

/** The search box; `query` is what's typed, `hits` its results (null while empty). */
export function useConversationSearch() {
  const { call } = useLyra();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<SearchHit[] | null>(null);
  useEffect(() => {
    const q = query.trim();
    if (!q) {
      setHits(null);
      return;
    }
    // Wait for a pause in typing.
    const t = window.setTimeout(() => void call<SearchHit[]>("search", { query: q }).then((h) => setHits(Array.isArray(h) ? h : [])), 300);
    return () => window.clearTimeout(t);
  }, [query, call]);
  return { query, setQuery, hits };
}

export function SearchBox({ query, setQuery, className }: { query: string; setQuery: (q: string) => void; className?: string }) {
  return (
    <div className={cn("relative", className)}>
      <Search className="-translate-y-1/2 absolute top-1/2 left-2.5 size-3.5 text-muted-foreground" />
      <Input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search conversations…" className="h-8 pr-7 pl-8 text-sm" />
      {query && (
        <button type="button" onClick={() => setQuery("")} className="-translate-y-1/2 absolute top-1/2 right-2 text-muted-foreground hover:text-foreground" aria-label="Clear search">
          <X className="size-3.5" />
        </button>
      )}
    </div>
  );
}

/** Matching conversations with the line that matched; `open` resumes one. */
export function SearchHits({ hits, open }: { hits: SearchHit[]; open: (id: string, current: boolean) => void }) {
  if (hits.length === 0) return <p className="px-3 py-2 text-muted-foreground text-xs">Nothing found.</p>;
  return (
    <>
      {hits.map((h) => (
        <button key={h.id} type="button" onClick={() => open(h.id, h.current)} className={cn("w-full rounded-lg px-3 py-1.5 text-left hover:bg-accent/50", h.current && "bg-accent")}>
          <span className="block truncate text-sm">{h.title || "(untitled)"}</span>
          <span className="line-clamp-2 block text-muted-foreground text-xs">
            <span className="text-muted-foreground/70">{h.role}: </span>
            {h.snippet}
          </span>
          <span className="block text-muted-foreground/60 text-[11px]">{ago(h.updated)}</span>
        </button>
      ))}
    </>
  );
}
