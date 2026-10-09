// Saved prompts by the message box: one tap fills it (to change before
// sending). Your own, and shared ones an admin publishes; save what you've
// typed as a new one, edit or remove yours (admins: the shared ones too).

import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { Bookmark, Pencil, Plus, Trash2, Users } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import { useLyra } from "./store";

interface Template {
  id: string;
  title: string;
  prompt: string;
}
interface Templates {
  mine: Template[];
  shared: Template[];
  can_share: boolean;
  error?: string;
}
type Editing = { id: string; title: string; prompt: string; shared: boolean };

export function TemplatesButton({ text, use }: { text: string; use: (prompt: string) => void }) {
  const { call } = useLyra();
  const [open, setOpen] = useState(false);
  const [data, setData] = useState<Templates | null>(null);
  const [editing, setEditing] = useState<Editing | null>(null);
  const [error, setError] = useState("");
  const [filter, setFilter] = useState("");
  const load = useCallback(() => void call<Templates>("templates").then(setData), [call]);
  useEffect(() => {
    if (open) load();
  }, [open, load]);

  const save = async () => {
    if (!editing) return;
    const r = await call<Templates>("template_put", editing);
    if (r?.error) return setError(r.error);
    setData(r);
    setEditing(null);
    setError("");
  };
  const remove = async (t: Template, shared: boolean) => {
    const r = await call<Templates>("template_remove", { id: t.id, shared });
    if (r?.error) return setError(r.error);
    setData(r);
  };
  const pick = (t: Template) => {
    use(t.prompt);
    setOpen(false);
  };
  const match = (t: Template) => !filter.trim() || `${t.title} ${t.prompt}`.toLowerCase().includes(filter.trim().toLowerCase());

  const row = (t: Template, shared: boolean) => {
    const mayEdit = !shared || data?.can_share;
    return (
      <div key={`${shared}-${t.id}`} className="group flex items-start gap-1 rounded-md hover:bg-accent/60">
        <button type="button" onClick={() => pick(t)} className="min-w-0 flex-1 px-2 py-1.5 text-left">
          <div className="font-medium text-sm">{t.title}</div>
          <div className="line-clamp-2 text-muted-foreground text-xs">{t.prompt}</div>
        </button>
        {mayEdit && (
          <div className="flex shrink-0 gap-0.5 pt-1 pr-1 opacity-70 group-hover:opacity-100">
            <Button size="icon" variant="ghost" className="size-7" aria-label={`Edit ${t.title}`} onClick={() => setEditing({ ...t, shared })}>
              <Pencil className="size-3.5" />
            </Button>
            <Button size="icon" variant="ghost" className="size-7" aria-label={`Remove ${t.title}`} onClick={() => void remove(t, shared)}>
              <Trash2 className="size-3.5" />
            </Button>
          </div>
        )}
      </div>
    );
  };

  const mine = (data?.mine ?? []).filter(match);
  const shared = (data?.shared ?? []).filter(match);
  return (
    <>
      <Button type="button" size="icon-sm" variant="ghost" aria-label="Saved prompts" title="Saved prompts" onClick={() => setOpen(true)}>
        <Bookmark className="size-4" />
      </Button>
      <Dialog
        open={open}
        onOpenChange={(o) => {
          setOpen(o);
          if (!o) setEditing(null);
        }}
      >
        <DialogContent className="max-h-[85dvh] overflow-y-auto sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>{editing ? (editing.id ? "Edit saved prompt" : "Save a prompt") : "Saved prompts"}</DialogTitle>
            <DialogDescription>{editing ? "A name to find it by, and what goes in the message box." : "Tap one to put it in the message box, then change it before sending."}</DialogDescription>
          </DialogHeader>
          {error && <p className="text-red-300 text-sm">{error}</p>}
          {editing ? (
            <div className="space-y-2">
              <Input value={editing.title} onChange={(e) => setEditing({ ...editing, title: e.target.value })} placeholder="Weekly status for my manager" aria-label="Name" />
              <Textarea value={editing.prompt} onChange={(e) => setEditing({ ...editing, prompt: e.target.value })} rows={6} placeholder="What lyra should do…" aria-label="Prompt" />
              {data?.can_share && (
                <label className="flex items-center gap-2 text-sm">
                  <input type="checkbox" checked={editing.shared} disabled={!!editing.id} onChange={(e) => setEditing({ ...editing, shared: e.target.checked })} />
                  <Users className="size-4" /> Share with everyone
                </label>
              )}
              <DialogFooter>
                <Button variant="ghost" onClick={() => setEditing(null)}>
                  Cancel
                </Button>
                <Button onClick={() => void save()} disabled={!editing.title.trim() || !editing.prompt.trim()}>
                  Save
                </Button>
              </DialogFooter>
            </div>
          ) : (
            <div className="space-y-3">
              <div className="flex gap-2">
                <Input value={filter} onChange={(e) => setFilter(e.target.value)} placeholder="Find a prompt" aria-label="Find a prompt" />
                <Button variant="secondary" onClick={() => setEditing({ id: "", title: "", prompt: text, shared: false })}>
                  <Plus /> {text.trim() ? "Save this" : "New"}
                </Button>
              </div>
              <section>
                <h3 className="mb-1 font-medium text-muted-foreground text-xs uppercase">Yours</h3>
                {mine.length ? mine.map((t) => row(t, false)) : <p className="px-2 text-muted-foreground text-sm">None yet. Type a message you send often, then Save this.</p>}
              </section>
              <section className={cn(!shared.length && "hidden")}>
                <h3 className="mb-1 flex items-center gap-1 font-medium text-muted-foreground text-xs uppercase">
                  <Users className="size-3" /> Shared
                </h3>
                {shared.map((t) => row(t, true))}
              </section>
            </div>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
