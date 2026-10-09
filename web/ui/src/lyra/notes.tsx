// Notes and lists: the person's own (Markdown files on the server). Changes
// go through lyra's commands; lists tick off with a tap.

import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { cn } from "@/lib/utils";
import { CheckSquare, Plus, Square, Trash2 } from "lucide-react";
import { useState, type FormEvent } from "react";
import { takeIntent } from "./intent";
import { Back, Failed, Page, useAction, useConfirm } from "./parts";
import { useData } from "./store";

interface NoteView {
  title: string;
  updated: string;
  list: boolean;
  items: { done: boolean; text: string }[];
  text: string;
}

export function NotesPage({ onBack }: { onBack: () => void }) {
  const [data, reload] = useData<NoteView[] | { error: string }>("notes");
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
  return (
    <Page title="Notes" description={'Your notes and lists. Or just tell lyra: "add milk to groceries", "note that the gate code is 4411".'} action={<Back onBack={onBack} />}>
      {data && !Array.isArray(data) && <Failed error={data.error} />}
      {note}
      <form onSubmit={add} className="flex gap-2">
        <Input value={text} onChange={(e) => setText(e.target.value)} placeholder="ideas: new laptop policy  ·  groceries: milk, eggs" disabled={busy} autoFocus={addFocus} />
        <Button type="submit" disabled={busy || !text.trim()}>
          <Plus /> Add
        </Button>
      </form>
      {data && notes.length === 0 && <p className="text-muted-foreground text-sm">No notes yet.</p>}
      <div className="grid gap-4 md:grid-cols-2">
        {notes.map((n) => (
          <Card key={n.title} className="gap-1 py-3">
            <CardHeader className="px-4">
              <CardTitle className="text-sm">{n.title}</CardTitle>
              <CardAction className="flex items-center gap-1">
                <span className="text-muted-foreground text-xs">{n.updated}</span>
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
                <p className="whitespace-pre-wrap">{n.text}</p>
              )}
            </CardContent>
          </Card>
        ))}
      </div>
      {dialog}
    </Page>
  );
}
