// Notes: the person's own notes, lists and documents (Markdown files on the
// server). A quick line adds a note or to a list; lists tick off with a tap;
// any note opens in the editor, where lyra drafts and revises it and it goes
// to Word, OneDrive or a mail draft.

import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { Badge } from "@/components/ui/badge";
import { ArrowLeft, CheckSquare, FilePlus, FileText, ListChecks, Plus, Square, SquarePen, StickyNote, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { Editor } from "./documents";
import { takeIntent } from "./intent";
import { Back, Failed, Page, useAction, useConfirm } from "./parts";
import { useData, useLyra } from "./store";

interface NoteView {
  /** A document (started as one, or switched), a list (checklist lines) or a note. */
  kind: "document" | "list" | "note";
  slug: string;
  title: string;
  words: number;
  updated: string;
  list: boolean;
  items: { done: boolean; text: string }[];
  text: string;
}

const KIND = {
  document: { label: "Document", icon: FileText, cls: "border-sky-700/60 text-sky-300" },
  list: { label: "List", icon: ListChecks, cls: "border-teal-700/60 text-teal-300" },
  note: { label: "Note", icon: StickyNote, cls: "border-amber-700/60 text-amber-300" },
};

export function NotesPage({ onBack }: { onBack: () => void }) {
  const { call } = useLyra();
  const [data, reload] = useData<NoteView[] | { error: string }>("notes");
  // The note open in the editor (its file name), if any.
  const [open, setOpen] = useState<string | null>(null);
  const [failed, setFailed] = useState("");
  const { act, busy, note } = useAction(reload);
  const [confirm, dialog] = useConfirm();
  const [text, setText] = useState("");
  // Opened from the Quick note shortcut: straight into the box.
  const [addFocus] = useState(() => takeIntent("add"));
  const notes = Array.isArray(data) ? data : [];
  // "groceries: milk, eggs" adds to a list; "title: text" notes; plain text is a dated note.
  const add = async (e: FormEvent) => {
    e.preventDefault();
    const t = text.trim();
    if (!t) return;
    const [head, ...rest] = t.split(":");
    const body = rest.join(":").trim();
    const list = notes.find((n) => n.list && n.title.toLowerCase() === head.trim().toLowerCase());
    const ok = list && body ? await act(`/list ${head.trim()} add ${body}`) : await act(`/note ${t}`);
    if (ok) setText("");
  };
  const create = async () => {
    const r = await call<{ id?: string; error?: string }>("document_save", { id: "", text: "# Untitled\n\n" });
    if (r?.id) {
      reload();
      setOpen(r.id);
    } else setFailed(r?.error ?? "couldn't make one");
  };
  if (open)
    return (
      <Page title="Notes" description="Write it yourself, or ask lyra to draft or change it. Save it to OneDrive as Word, or attach it to an email." action={<Back onBack={onBack} />}>
        <div className="space-y-2">
          <Button size="sm" variant="ghost" onClick={() => (setOpen(null), reload())}>
            <ArrowLeft /> All notes
          </Button>
          <Editor key={open} id={open} document={notes.find((n) => n.slug === open)?.kind === "document"} onSaved={reload} onGone={() => (setOpen(null), reload())} />
        </div>
      </Page>
    );
  return (
    <Page title="Notes" description={'Your notes, lists and documents. Or just tell lyra: "add milk to groceries", "note that the gate code is 4411". Open one to write it with lyra.'} action={<Back onBack={onBack} />}>
      {data && !Array.isArray(data) && <Failed error={data.error} />}
      <Failed error={failed || undefined} />
      {note}
      <form onSubmit={add} className="flex gap-2">
        <Input value={text} onChange={(e) => setText(e.target.value)} placeholder="ideas: new laptop policy  ·  groceries: milk, eggs" disabled={busy} autoFocus={addFocus} />
        <Button type="submit" disabled={busy || !text.trim()}>
          <Plus /> Add
        </Button>
        <Button type="button" variant="secondary" onClick={() => void create()} title="A new note in the editor: a letter, memo or one-pager, written with lyra">
          <FilePlus /> <span className="hidden sm:inline">New document</span>
        </Button>
      </form>
      {data && notes.length === 0 && <p className="text-muted-foreground text-sm">No notes yet.</p>}
      <div className="grid gap-4 md:grid-cols-2">
        {notes.map((n) => (
          <Card key={n.title} className="gap-1 py-3">
            <CardHeader className="px-4">
              <CardTitle className="flex min-w-0 items-center gap-2 text-sm">
                {(() => {
                  const k = KIND[n.kind] ?? KIND.note;
                  return (
                    <Badge variant="outline" className={cn("shrink-0 gap-1 font-normal", k.cls)}>
                      <k.icon className="size-3" /> {k.label}
                    </Badge>
                  );
                })()}
                <button type="button" className="min-w-0 truncate text-left hover:underline" onClick={() => setOpen(n.slug)}>
                  {n.title}
                </button>
              </CardTitle>
              <CardAction className="flex items-center gap-1">
                <span className="text-muted-foreground text-xs">{n.updated}</span>
                <Button size="icon" variant="ghost" className="size-7" aria-label="Open in the editor" onClick={() => setOpen(n.slug)}>
                  <SquarePen />
                </Button>
                <Button
                  size="icon"
                  variant="ghost"
                  className="size-7"
                  aria-label="Delete"
                  onClick={() => confirm({ title: `Delete ${n.title}?`, text: "It's gone for good.", action: "Delete", run: () => void act(`/note delete ${n.title}`) })}
                >
                  <Trash2 />
                </Button>
              </CardAction>
            </CardHeader>
            <CardContent className="px-4 text-sm">
              {n.list ? (
                <div className="space-y-0.5">
                  {n.items.map((it) => (
                    <button
                      key={it.text}
                      type="button"
                      disabled={busy}
                      onClick={() => void act(`/list ${n.title} ${it.done ? "undone" : "done"} ${it.text}`)}
                      className={cn("flex w-full items-center gap-2 py-0.5 text-left", it.done && "text-muted-foreground line-through")}
                    >
                      {it.done ? <CheckSquare className="size-4 shrink-0 text-teal-400" /> : <Square className="size-4 shrink-0" />}
                      {it.text}
                    </button>
                  ))}
                </div>
              ) : (
                <button type="button" className="line-clamp-6 w-full whitespace-pre-wrap text-left" onClick={() => setOpen(n.slug)}>
                  {n.text || <span className="text-muted-foreground">Empty</span>}
                  {n.words > 120 && <span className="block text-muted-foreground text-xs">{n.words} words</span>}
                </button>
              )}
            </CardContent>
          </Card>
        ))}
      </div>
      {dialog}
    </Page>
  );
}
