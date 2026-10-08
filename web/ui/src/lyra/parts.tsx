// Pieces every page uses: the page frame, an online dot, a confirm dialog,
// the back button, an error line and running a command from a page.

import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { cn } from "@/lib/utils";
import { ArrowLeft, X } from "lucide-react";
import { useCallback, useState, type ReactNode } from "react";
import { useLyra } from "./store";

export function Page({ title, description, action, children }: { title: string; description?: string; action?: ReactNode; children: ReactNode }) {
  // dashboard-01's main area: a container with its spacing, and its cards'
  // soft gradient on every card directly in the page.
  return (
    <div className="@container/main min-h-0 flex-1 overflow-y-auto">
      <div className="mx-auto flex max-w-5xl flex-col gap-4 px-4 py-4 *:data-[slot=card]:bg-gradient-to-t *:data-[slot=card]:from-primary/5 *:data-[slot=card]:to-card *:data-[slot=card]:shadow-xs md:gap-6 md:py-6 lg:px-6 dark:*:data-[slot=card]:bg-card">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h1 className="font-semibold text-xl">{title}</h1>
            {description && <p className="text-muted-foreground text-sm">{description}</p>}
          </div>
          {action}
        </div>
        {children}
      </div>
    </div>
  );
}

export function Dot({ on }: { on: boolean }) {
  return <span className={cn("inline-block size-2.5 shrink-0 rounded-full", on ? "bg-emerald-500 shadow-[0_0_8px] shadow-emerald-500/60" : "bg-muted-foreground/40")} />;
}

/** Ask before something that can't be undone. */
export function useConfirm() {
  const [ask, setAsk] = useState<{ title: string; text: string; action: string; run: () => void } | null>(null);
  const dialog = (
    <Dialog open={!!ask} onOpenChange={(o) => !o && setAsk(null)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{ask?.title}</DialogTitle>
          <DialogDescription>{ask?.text}</DialogDescription>
        </DialogHeader>
        <DialogFooter>
          <Button variant="outline" onClick={() => setAsk(null)}>Cancel</Button>
          <Button
            variant="destructive"
            onClick={() => {
              ask?.run();
              setAsk(null);
            }}
          >
            {ask?.action}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
  return [setAsk, dialog] as const;
}

/** Run a command, show lyra's answer, reload the page. */
export function useAction(reload: () => void) {
  const { run } = useLyra();
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const act = useCallback(
    async (command: string) => {
      setBusy(true);
      const r = await run(command);
      setBusy(false);
      setNote(r);
      reload();
      return r.ok;
    },
    [run, reload],
  );
  const shown = note && (
    <div className={cn("flex items-start justify-between gap-2 rounded-md border px-3 py-2 text-sm", note.ok ? "border-teal-700/60 bg-teal-950/30 text-teal-100" : "border-red-800/60 bg-red-950/30 text-red-200")}>
      <span className="whitespace-pre-wrap break-words">{note.text}</span>
      <button type="button" onClick={() => setNote(null)} className="text-muted-foreground hover:text-foreground">
        <X className="size-4" />
      </button>
    </div>
  );
  return { act, busy, note: shown };
}

export function Back({ onBack }: { onBack: () => void }) {
  return (
    // Wide screens have the sidebar instead.
    <Button size="icon" variant="ghost" onClick={onBack} aria-label="Back" className="md:hidden">
      <ArrowLeft />
    </Button>
  );
}

export function Failed({ error }: { error?: string }) {
  return error ? <p className="rounded-md border border-red-800/60 bg-red-950/30 px-3 py-2 text-red-200 text-sm">{error}</p> : null;
}
