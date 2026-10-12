// How a reply went, from the person: 👍 / 👎 under each reply (👎 asks what
// was wrong, optional), and Copy. Each goes to lyra as a "rate" message;
// copying or having a reply read aloud are hints it helped. lyra's skills,
// memories, agents and evolution all learn from them.

import { cn } from "@/lib/utils";
import { Check, Copy, FlaskConical, Send, ThumbsDown, ThumbsUp } from "lucide-react";
import { useState, type FormEvent } from "react";
import { useLyra } from "./store";

/** Tell lyra how reply `index` went (`good`, `bad`; `copied`, `read`). */
export function useRate() {
  const { send } = useLyra();
  return (index: number, rating: string, note?: string) => send({ type: "rate", index, rating, ...(note ? { note } : {}) });
}

export function CopyButton({ index, text }: { index: number; text: string }) {
  const rate = useRate();
  const [done, setDone] = useState(false);
  if (!text.trim() || !navigator.clipboard) return null;
  const copy = () =>
    void navigator.clipboard.writeText(text).then(() => {
      setDone(true);
      rate(index, "copied");
      window.setTimeout(() => setDone(false), 1500);
    });
  return (
    <button type="button" aria-label="Copy" title="Copy" onClick={copy} className="rounded p-0.5 hover:text-foreground">
      {done ? <Check className="size-3.5 text-emerald-400" /> : <Copy className="size-3.5" />}
    </button>
  );
}

/** An admin's "add to tests": this request and reply join the test set lyra's changes are measured on. */
export function AddTestButton({ index }: { index: number }) {
  const rate = useRate();
  const { user } = useLyra();
  const [added, setAdded] = useState(false);
  if (!(user?.admin ?? true)) return null;
  return (
    <button
      type="button"
      aria-label="Add to tests"
      title={added ? "Added to lyra's tests" : "Add to lyra's tests: changes to lyra are measured on this request and answer (other admins can see it)"}
      disabled={added}
      onClick={() => rate(index, "test") && setAdded(true)}
      className={cn("rounded p-0.5 hover:text-foreground", added && "text-sky-400")}
    >
      <FlaskConical className="size-3.5" />
    </button>
  );
}

export function RateButtons({ index, rated }: { index: number; rated?: string | null }) {
  const rate = useRate();
  const [asking, setAsking] = useState(false);
  const [note, setNote] = useState("");
  const [sent, setSent] = useState(false);
  const up = rated === "good";
  const down = rated === "bad";
  const tell = (e: FormEvent) => {
    e.preventDefault();
    if (note.trim() && rate(index, "bad", note.trim())) {
      setSent(true);
      setAsking(false);
      setNote("");
    }
  };
  return (
    <>
      <button type="button" aria-label="Good reply" aria-pressed={up} title="Good reply" onClick={() => (rate(index, "good"), setAsking(false))} className={cn("rounded p-0.5 hover:text-foreground", up && "text-emerald-400")}>
        <ThumbsUp className="size-3.5" />
      </button>
      <button
        type="button"
        aria-label="Bad reply"
        aria-pressed={down}
        title="Bad reply: say what was wrong, and lyra learns from it"
        onClick={() => {
          rate(index, "bad");
          setSent(false);
          setAsking(true);
        }}
        className={cn("rounded p-0.5 hover:text-foreground", down && "text-red-400")}
      >
        <ThumbsDown className="size-3.5" />
      </button>
      {asking && (
        <form onSubmit={tell} className="flex w-full items-center gap-1.5 sm:w-auto">
          <input
            autoFocus
            value={note}
            onChange={(e) => setNote(e.target.value)}
            onKeyDown={(e) => e.key === "Escape" && setAsking(false)}
            placeholder="What was wrong? (optional)"
            aria-label="What was wrong"
            maxLength={500}
            className="h-7 min-w-0 flex-1 rounded-md border bg-background px-2 text-foreground text-xs sm:w-72"
          />
          <button type="submit" disabled={!note.trim()} aria-label="Tell lyra" title="Tell lyra" className="rounded p-1 hover:text-foreground disabled:opacity-40">
            <Send className="size-3.5" />
          </button>
        </form>
      )}
      {sent && !asking && <span className="text-muted-foreground">thanks: lyra will learn from it</span>}
    </>
  );
}
