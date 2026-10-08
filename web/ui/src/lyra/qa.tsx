// Q&A: questions people asked about lyra, with the answers the admins
// approved. Everyone reads it; admins add, change and remove entries
// (questions from Feedback are promoted here with lyra's drafted answer).

import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { ChevronDown, CircleHelp, Pencil, Plus, Search, Trash2 } from "lucide-react";
import { useState } from "react";
import { MessageResponse } from "@/components/ai-elements/message";
import { Back, Failed } from "./manage";
import { Page, useConfirm } from "./parts";
import { ago } from "./push";
import { useData, useLyra } from "./store";

interface Entry {
  id: number;
  question: string;
  answer: string;
  source: number | null;
  asked_by: string | null;
  approved_by: string;
  created: string;
  updated: string;
}

function Editor({ start, save, cancel }: { start: { question: string; answer: string }; save: (q: string, a: string) => Promise<string | null>; cancel: () => void }) {
  const [q, setQ] = useState(start.question);
  const [a, setA] = useState(start.answer);
  const [err, setErr] = useState("");
  return (
    <div className="space-y-2">
      <Input value={q} onChange={(e) => setQ(e.target.value)} placeholder="The question" />
      <Textarea value={a} onChange={(e) => setA(e.target.value)} rows={7} placeholder="The answer (Markdown works)" />
      <div className="flex items-center gap-2">
        <Button size="sm" disabled={q.trim().length < 3 || a.trim().length < 3} onClick={async () => setErr((await save(q, a)) ?? "")}>
          Save
        </Button>
        <Button size="sm" variant="ghost" onClick={cancel}>
          Cancel
        </Button>
        {err && <span className="text-red-400 text-xs">{err}</span>}
      </div>
    </div>
  );
}

export function QAPage({ onBack }: { onBack: () => void }) {
  const { call } = useLyra();
  const [data, reload] = useData<{ admin: boolean; entries: Entry[]; error?: string } | null>("qa");
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState<number | null>(null);
  const [editing, setEditing] = useState<number | null>(null);
  const [confirm, dialog] = useConfirm();
  const save = async (id: number, question: string, answer: string) => {
    const r = await call<{ ok?: boolean; error?: string }>("qa_put", { id, question, answer });
    if (r.error) return r.error;
    setEditing(null);
    reload();
    return null;
  };
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  const shown = (data?.entries ?? []).filter((e) => words.every((w) => `${e.question} ${e.answer}`.toLowerCase().includes(w)));
  return (
    <Page title="Q&A" description="Questions people asked about lyra, and the answers. Ask yours on the Feedback page." action={<Back onBack={onBack} />}>
      <Failed error={data?.error} />
      <div className="flex gap-2">
        <div className="relative flex-1">
          <Search className="absolute top-2.5 left-2.5 size-4 text-muted-foreground" />
          <Input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="Search the questions and answers…" className="pl-8" />
        </div>
        {data?.admin && (
          <Button variant="outline" onClick={() => setEditing(0)}>
            <Plus /> Add
          </Button>
        )}
      </div>
      {editing === 0 && (
        <Card className="py-4">
          <CardContent className="px-4">
            <Editor start={{ question: "", answer: "" }} save={(q, a) => save(0, q, a)} cancel={() => setEditing(null)} />
          </CardContent>
        </Card>
      )}
      {data && shown.length === 0 && <p className="text-muted-foreground text-sm">{query ? "Nothing matches." : "No questions answered yet."}</p>}
      {shown.map((e) => (
        <Card key={e.id} className="gap-2 py-3">
          <button type="button" onClick={() => setOpen(open === e.id ? null : e.id)} className="flex w-full items-start gap-3 px-4 text-left">
            <CircleHelp className="mt-0.5 size-4 shrink-0 text-sky-400" />
            <span className="min-w-0 flex-1 font-medium text-sm">{e.question}</span>
            <ChevronDown className={cn("mt-0.5 size-4 shrink-0 text-muted-foreground transition-transform", open === e.id && "rotate-180")} />
          </button>
          {open === e.id && (
            <CardContent className="space-y-2 px-4">
              {editing === e.id ? (
                <Editor start={e} save={(q, a) => save(e.id, q, a)} cancel={() => setEditing(null)} />
              ) : (
                <>
                  <div className="text-sm">
                    <MessageResponse>{e.answer}</MessageResponse>
                  </div>
                  <div className="flex flex-wrap items-center gap-2 text-muted-foreground text-xs">
                    {e.asked_by && <span>asked by {e.asked_by}</span>}
                    <span>answered by {e.approved_by}</span>
                    <span>{ago(e.updated)}</span>
                    {data?.admin && (
                      <span className="ml-auto flex gap-1">
                        <Button size="icon" variant="ghost" aria-label="Edit" onClick={() => setEditing(e.id)}>
                          <Pencil />
                        </Button>
                        <Button
                          size="icon"
                          variant="ghost"
                          aria-label="Remove"
                          onClick={() =>
                            confirm({
                              title: "Remove this from Q&A?",
                              text: "Everyone stops seeing it.",
                              action: "Remove",
                              run: () => void call("qa_remove", { id: e.id }).then(reload),
                            })
                          }
                        >
                          <Trash2 />
                        </Button>
                      </span>
                    )}
                  </div>
                </>
              )}
            </CardContent>
          )}
        </Card>
      ))}
      {dialog}
    </Page>
  );
}
