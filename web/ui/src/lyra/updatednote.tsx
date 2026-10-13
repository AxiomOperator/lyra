// The note after an update ("lyra was updated to …", with What's new): its
// own small file, so What's new itself loads only when it's opened.

import { Button } from "@/components/ui/button";
import { Sparkles, X } from "lucide-react";
import { useEffect, useState } from "react";
import { useLyra } from "./store";

const SEEN = "lyra-seen-version";

export function markSeen(v: string) {
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
