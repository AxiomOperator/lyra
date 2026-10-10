// The note editor (on the Notes page): write a note, letter, memo or
// one-pager side by side with lyra. Your text on one side (saved as you type;
// a new title renames it); on the other, ask lyra to draft or change it and
// look at its version before using it. A finished one goes to your OneDrive
// as Word, onto a mail draft, or downloads (nothing is sent).

import { MessageResponse } from "@/components/ai-elements/message";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { Check, CloudUpload, Download, Eye, FileText, Mail, Pencil, Sparkles, StickyNote, Trash2, X } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { useConfirm } from "./parts";
import { useLyra } from "./store";

const QUICK = ["Make it more formal", "Shorten it", "Fix the grammar and spelling", "Add a short closing", "Turn it into bullet points"];

/** One note in full, by its file name (`id`). Its name follows its title as it's saved. */
export function Editor({ id: opened, document: wasDocument, onSaved, onGone }: { id: string; document?: boolean; onSaved: () => void; onGone: () => void }) {
  const { call } = useLyra();
  const [text, setText] = useState<string | null>(null);
  const [saved, setSaved] = useState("");
  const [preview, setPreview] = useState(false);
  const [instruction, setInstruction] = useState("");
  const [proposal, setProposal] = useState<string | null>(null);
  const [busy, setBusy] = useState("");
  const [note, setNote] = useState<{ ok: boolean; text: string; url?: string } | null>(null);
  const [to, setTo] = useState("");
  const [confirm, dialog] = useConfirm();
  // A document or a plain note (switched here; the Notes page shows which).
  const [isDocument, setIsDocument] = useState(!!wasDocument);
  const switchKind = async () => {
    const r = await call<{ ok?: boolean; error?: string }>("document_kind", { id: name.current, document: !isDocument });
    if (r?.ok) {
      setIsDocument(!isDocument);
      onSaved();
    } else setNote({ ok: false, text: r?.error ?? "couldn't change it" });
  };
  const loaded = useRef(false);
  // Its file name now (a new title renames it while it's open).
  const name = useRef(opened);

  useEffect(() => {
    loaded.current = false;
    name.current = opened;
    void call<{ text?: string; error?: string }>("document", { id: opened }).then((d) => {
      setText(d?.text ?? "");
      loaded.current = true;
    });
  }, [opened, call]);
  // Saved a second after typing stops.
  useEffect(() => {
    if (text === null || !loaded.current) return;
    setSaved("Saving…");
    const t = window.setTimeout(() => {
      void call<{ id?: string; error?: string }>("document_save", { id: name.current, text }).then((r) => {
        setSaved(r?.id ? "Saved" : r?.error ?? "Not saved");
        if (r?.id) {
          name.current = r.id;
          onSaved();
        }
      });
    }, 1000);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text]);

  const ask = async (what: string) => {
    if (!what.trim()) return;
    setBusy("lyra is writing…");
    setNote(null);
    const r = await call<{ text?: string; error?: string }>("document_ask", { text: text ?? "", instruction: what });
    setBusy("");
    if (r?.text) setProposal(r.text);
    else setNote({ ok: false, text: r?.error ?? "lyra didn't answer" });
  };
  const act = async (what: "document_onedrive" | "document_mail" | "document_download") => {
    setBusy(what === "document_onedrive" ? "Saving to OneDrive…" : what === "document_mail" ? "Making the mail draft…" : "Making the Word file…");
    setNote(null);
    const r = await call<{ error?: string; url?: string; name?: string; base64?: string; mime?: string; subject?: string }>(what, { id: name.current, to: to.split(/[,;\s]+/).filter(Boolean) });
    setBusy("");
    if (r?.error) return setNote({ ok: false, text: r.error });
    if (what === "document_download" && r?.base64) {
      const bytes = Uint8Array.from(atob(r.base64), (c) => c.charCodeAt(0));
      const a = document.createElement("a");
      a.href = URL.createObjectURL(new Blob([bytes], { type: r.mime }));
      a.download = r.name ?? "Document.docx";
      a.click();
      URL.revokeObjectURL(a.href);
      return;
    }
    if (what === "document_onedrive") setNote({ ok: true, text: `Saved to your OneDrive as lyra/${r?.name}.`, url: r?.url });
    else setNote({ ok: true, text: `It's in your Outlook Drafts with the Word file attached${to.trim() ? "" : " (add who it's for there)"}. Look it over and send it from Outlook.` });
  };

  if (text === null) return <p className="text-muted-foreground text-sm">Opening…</p>;
  return (
    <div className="grid gap-4 xl:grid-cols-[1fr_22rem]">
      <div className="min-w-0 space-y-2">
        <div className="flex flex-wrap items-center gap-2">
          <Button size="sm" variant={preview ? "secondary" : "ghost"} onClick={() => setPreview(!preview)}>
            {preview ? <Pencil /> : <Eye />} {preview ? "Edit" : "Preview"}
          </Button>
          <span className="text-muted-foreground text-xs">{saved}</span>
          <Button size="sm" variant="ghost" onClick={() => void switchKind()} title={isDocument ? "Show it as a plain note" : "Show it as a document"}>
            {isDocument ? <StickyNote /> : <FileText />} {isDocument ? "Make it a note" : "Make it a document"}
          </Button>
          <div className="ml-auto flex flex-wrap gap-1">
            <Button size="sm" variant="ghost" onClick={() => void act("document_download")} disabled={!!busy}>
              <Download /> Word
            </Button>
            <Button size="sm" variant="ghost" onClick={() => void act("document_onedrive")} disabled={!!busy}>
              <CloudUpload /> Save to OneDrive
            </Button>
            <Button
              size="sm"
              variant="ghost"
              aria-label="Delete the note"
              onClick={() =>
                confirm({ title: "Delete this note?", text: "It's gone for good (copies saved to OneDrive or mail stay).", action: "Delete", run: () => void call("document_remove", { id: name.current }).then(onGone) })
              }
            >
              <Trash2 />
            </Button>
          </div>
        </div>
        {preview ? (
          <div className="min-h-[24rem] rounded-md border p-4">
            <MessageResponse>{text || "_Nothing yet._"}</MessageResponse>
          </div>
        ) : (
          <Textarea value={text} onChange={(e) => setText(e.target.value)} rows={22} className="min-h-[24rem] max-h-[70dvh] font-mono text-sm" placeholder={"# Title\n\nWrite here, or ask lyra to draft it →"} aria-label="Note" />
        )}
        <div className="flex flex-wrap items-center gap-2">
          <Input value={to} onChange={(e) => setTo(e.target.value)} placeholder="To (optional): dana@fbcad.org" className="max-w-xs" aria-label="Mail it to" />
          <Button size="sm" variant="secondary" onClick={() => void act("document_mail")} disabled={!!busy}>
            <Mail /> Attach to an email
          </Button>
        </div>
        {note && (
          <p className={cn("rounded-md border px-3 py-2 text-sm", note.ok ? "border-teal-700/60" : "border-red-800/60 text-red-200")}>
            {note.text}{" "}
            {note.url && (
              <a href={note.url} target="_blank" rel="noreferrer" className="underline">
                Open it
              </a>
            )}
          </p>
        )}
      </div>
      <Card className="h-fit gap-2 py-4">
        <CardHeader className="px-4">
          <CardTitle className="flex items-center gap-2 text-base">
            <Sparkles className="size-4" /> Ask lyra
          </CardTitle>
          <CardDescription>{text.trim() ? "What should change?" : "What should it be? e.g. a letter to a property owner about…"}</CardDescription>
        </CardHeader>
        <CardContent className="space-y-2 px-4">
          <Textarea
            value={instruction}
            onChange={(e) => setInstruction(e.target.value)}
            rows={3}
            placeholder={text.trim() ? "Make the second paragraph friendlier" : "A one-page memo to staff: the office closes early Friday for the holiday"}
            aria-label="What lyra should do"
          />
          <Button onClick={() => void ask(instruction)} disabled={!!busy || !instruction.trim()} className="w-full">
            <Sparkles /> {text.trim() ? "Change it" : "Draft it"}
          </Button>
          {text.trim() && (
            <div className="flex flex-wrap gap-1">
              {QUICK.map((q) => (
                <button key={q} type="button" className="rounded-full border px-2 py-0.5 text-xs hover:bg-accent" onClick={() => void ask(q)} disabled={!!busy}>
                  {q}
                </button>
              ))}
            </div>
          )}
          {busy && <p className="text-muted-foreground text-xs">{busy}</p>}
          {proposal !== null && (
            <div className="space-y-2 rounded-md border border-teal-700/60 p-2">
              <p className="font-medium text-xs">lyra's version</p>
              <div className="max-h-80 overflow-y-auto text-sm">
                <MessageResponse>{proposal}</MessageResponse>
              </div>
              <div className="flex gap-2">
                <Button
                  size="sm"
                  onClick={() => {
                    setText(proposal);
                    setProposal(null);
                    setInstruction("");
                  }}
                >
                  <Check /> Use this
                </Button>
                <Button size="sm" variant="ghost" onClick={() => setProposal(null)}>
                  <X /> Discard
                </Button>
              </div>
            </div>
          )}
        </CardContent>
      </Card>
      {dialog}
    </div>
  );
}
