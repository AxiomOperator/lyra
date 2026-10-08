// Feedback: bug reports and feature requests for the admins. Two parts on
// one page: Submit (send one, with screenshots) and Track (follow yours —
// admins: everyone's — with status, priority, the version it shipped in and
// a comment thread).

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/textarea";
import { cn } from "@/lib/utils";
import { Bug, ChevronDown, Lightbulb, Loader2, Paperclip, RefreshCw, Send, Sparkles, X } from "lucide-react";
import { useCallback, useEffect, useState, type FormEvent } from "react";
import { upload } from "./chat";
import { SentAttachments } from "./chat-parts";
import { Back, Failed } from "./manage";
import { Page } from "./parts";
import { ago } from "./push";
import { useLyra } from "./store";

interface Comment {
  by: string;
  name: string;
  admin: boolean;
  text: string;
  at: string;
  system: boolean;
}
interface Item {
  id: number;
  kind: "bug" | "feature";
  title: string;
  details: string;
  severity: string | null;
  status: string;
  priority: string;
  user: string;
  name: string;
  created: string;
  updated: string;
  version: string;
  page: string;
  files: { id: string; name: string; mime: string }[];
  shipped_in: string | null;
  comments: Comment[];
  news_for_sender: boolean;
  news_for_admins: boolean;
  /** lyra's read: everyone gets the summary; admins the rest. */
  analysis?: { summary: string; cause?: string | null; approach?: string[]; effort?: string | null; area?: string | null; questions?: string[]; at: string; error?: string | null } | null;
  analyzing?: boolean;
}
interface Data {
  admin: boolean;
  me: string;
  version: string;
  items: Item[];
  error?: string;
}

const STATUS: Record<string, { label: string; cls: string }> = {
  new: { label: "New", cls: "bg-sky-500/15 text-sky-300" },
  reviewing: { label: "Reviewing", cls: "bg-amber-500/15 text-amber-300" },
  planned: { label: "Planned", cls: "bg-primary/15 text-primary" },
  done: { label: "Done", cls: "bg-emerald-500/15 text-emerald-300" },
  wontdo: { label: "Won't do", cls: "bg-muted text-muted-foreground" },
};
const PRIORITY: Record<string, string> = { low: "text-muted-foreground", normal: "text-foreground", high: "text-amber-300", urgent: "text-red-400" };

/** A small choice of words, as buttons. */
function Choice({ options, value, set }: { options: [string, string][]; value: string; set: (v: string) => void }) {
  return (
    <div className="flex flex-wrap gap-1">
      {options.map(([v, label]) => (
        <button key={v} type="button" onClick={() => set(v)} className={cn("rounded-md border px-2.5 py-1 text-sm", value === v ? "border-primary/50 bg-primary/15 text-primary" : "hover:bg-muted")}>
          {label}
        </button>
      ))}
    </div>
  );
}

function SubmitCard({ sent, page }: { sent: () => void; page: string }) {
  const { call, token } = useLyra();
  const [kind, setKind] = useState("bug");
  const [title, setTitle] = useState("");
  const [details, setDetails] = useState("");
  const [severity, setSeverity] = useState("annoying");
  const [files, setFiles] = useState<File[]>([]);
  const [busy, setBusy] = useState("");
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);
  // Enhance: lyra's clearer draft, shown to edit, used only if they say so.
  const [draft, setDraft] = useState<string | null>(null);
  const [enhancing, setEnhancing] = useState(false);
  const enhance = async () => {
    setEnhancing(true);
    setNote(null);
    const r = await call<{ text?: string; error?: string }>("feedback_enhance", { kind, title, details });
    setEnhancing(false);
    if (r.text) setDraft(r.text);
    else setNote({ ok: false, text: r.error ?? "Couldn't make a draft." });
  };
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setNote(null);
    try {
      const ids: string[] = [];
      for (const [i, f] of files.entries()) {
        setBusy(`Sending ${f.name} (${i + 1}/${files.length})…`);
        ids.push(await upload(token, f));
      }
      setBusy("Sending…");
      const r = await call<{ ok?: boolean; id?: number; error?: string }>("feedback_submit", { kind, title, details, severity: kind === "bug" ? severity : null, files: ids, page });
      if (r.error) throw new Error(r.error);
      setNote({ ok: true, text: `Sent as #${r.id}. You'll hear when it moves.` });
      setTitle("");
      setDetails("");
      setFiles([]);
      sent();
    } catch (err) {
      setNote({ ok: false, text: (err as Error).message });
    } finally {
      setBusy("");
    }
  };
  return (
    <Card className="gap-3 py-4">
      <CardHeader className="px-4">
        <CardTitle className="text-base">Send one</CardTitle>
        <CardDescription>Something broken, or something you'd like lyra to do. The admins see it; you follow it below.</CardDescription>
      </CardHeader>
      <CardContent className="px-4">
        <form onSubmit={submit} className="space-y-3">
          <Choice
            options={[
              ["bug", "🐞 Bug report"],
              ["feature", "💡 Feature request"],
            ]}
            value={kind}
            set={setKind}
          />
          <Input value={title} onChange={(e) => setTitle(e.target.value)} maxLength={140} placeholder={kind === "bug" ? "What's wrong, in a line" : "What you'd like, in a line"} />
          <div className="space-y-1.5">
            <Textarea
              value={details}
              onChange={(e) => setDetails(e.target.value)}
              rows={5}
              placeholder={kind === "bug" ? "What you did, what happened, what you expected. Steps help." : "What you'd use it for, and how you picture it working."}
            />
            <div className="flex items-center gap-2">
              <Button type="button" size="sm" variant="outline" className="border-primary/40 text-primary hover:text-primary" disabled={enhancing || details.trim().length < 5} onClick={() => void enhance()} title="lyra rewrites it more clearly, keeping what you said">
                {enhancing ? <Loader2 className="animate-spin" /> : <Sparkles />} {enhancing ? "Enhancing…" : "Enhance"}
              </Button>
              <span className="text-muted-foreground text-xs">A clearer draft from what you wrote; nothing changes until you use it.</span>
            </div>
            {draft !== null && (
              <div className="space-y-2 rounded-lg border border-primary/30 bg-primary/5 p-3">
                <div className="flex items-center gap-1.5 font-medium text-primary text-xs uppercase tracking-wide">
                  <Sparkles className="size-3.5" /> lyra's draft
                  <span className="font-normal text-muted-foreground normal-case tracking-normal">· edit it, then use it; [brackets] mark what you could add</span>
                </div>
                <Textarea value={draft} onChange={(e) => setDraft(e.target.value)} rows={7} />
                <div className="flex gap-2">
                  <Button
                    type="button"
                    size="sm"
                    onClick={() => {
                      setDetails(draft);
                      setDraft(null);
                    }}
                  >
                    Use this
                  </Button>
                  <Button type="button" size="sm" variant="ghost" onClick={() => setDraft(null)}>
                    Keep mine
                  </Button>
                </div>
              </div>
            )}
          </div>
          {kind === "bug" && (
            <div className="space-y-1">
              <div className="text-muted-foreground text-xs">How bad is it?</div>
              <Choice
                options={[
                  ["minor", "Minor"],
                  ["annoying", "Annoying"],
                  ["blocking", "Stops me working"],
                ]}
                value={severity}
                set={setSeverity}
              />
            </div>
          )}
          <div className="flex flex-wrap items-center gap-2">
            <label className="inline-flex cursor-pointer items-center gap-1.5 rounded-md border px-2.5 py-1 text-sm hover:bg-muted">
              <Paperclip className="size-4" /> Screenshots or files
              <input
                type="file"
                multiple
                hidden
                onChange={(e) => {
                  // Read now: the event is gone by the time state updates.
                  const picked = Array.from(e.currentTarget.files ?? []);
                  e.currentTarget.value = "";
                  setFiles((f) => [...f, ...picked]);
                }}
              />
            </label>
            {files.map((f, i) => (
              <span key={`${f.name}-${i}`} className="flex items-center gap-1 rounded-md bg-muted px-2 py-0.5 text-xs">
                {f.name}
                <button type="button" aria-label={`Remove ${f.name}`} onClick={() => setFiles((all) => all.filter((_, j) => j !== i))}>
                  <X className="size-3" />
                </button>
              </span>
            ))}
          </div>
          <div className="flex items-center gap-3">
            <Button type="submit" disabled={!!busy || title.trim().length < 3}>
              <Send /> Send
            </Button>
            {busy && <span className="text-muted-foreground text-xs">{busy}</span>}
            {note && <span className={cn("text-sm", note.ok ? "text-primary" : "text-red-400")}>{note.text}</span>}
          </div>
        </form>
      </CardContent>
    </Card>
  );
}

function ItemCard({ i, admin, me, version, reload }: { i: Item; admin: boolean; me: string; version: string; reload: () => void }) {
  const { call } = useLyra();
  const news = (i.user === me && i.news_for_sender) || (admin && i.news_for_admins);
  const [open, setOpen] = useState(false);
  const [text, setText] = useState("");
  const [shipped, setShipped] = useState(i.shipped_in ?? version);
  const toggle = () => {
    setOpen(!open);
    if (!open && news) void call("feedback_seen", { id: i.id }).then(reload);
  };
  const act = async (what: string, arg: Record<string, unknown>) => {
    await call(what, { id: i.id, ...arg });
    reload();
  };
  const st = STATUS[i.status] ?? STATUS.new;
  const Icon = i.kind === "bug" ? Bug : Lightbulb;
  return (
    <Card className={cn("gap-2 py-3", news && "border-primary/40")}>
      <button type="button" onClick={toggle} className="flex w-full items-start gap-3 px-4 text-left">
        <Icon className={cn("mt-0.5 size-4 shrink-0", i.kind === "bug" ? "text-red-400" : "text-amber-300")} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-mono text-muted-foreground text-xs">#{i.id}</span>
            <span className="font-medium text-sm">{i.title}</span>
            {news && <span className="size-2 rounded-full bg-primary" title="something new" />}
          </div>
          <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-muted-foreground text-xs">
            <Badge className={st.cls}>{st.label}</Badge>
            {i.status === "done" && i.shipped_in && <span>in lyra {i.shipped_in}</span>}
            <span className={PRIORITY[i.priority]}>{i.priority} priority</span>
            {admin && i.user !== me && <span>from {i.name}</span>}
            <span>{ago(i.updated)}</span>
            {i.comments.some((c) => !c.system) && <span>{i.comments.filter((c) => !c.system).length === 1 ? "1 comment" : `${i.comments.filter((c) => !c.system).length} comments`}</span>}
          </div>
        </div>
        <ChevronDown className={cn("mt-0.5 size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} />
      </button>
      {open && (
        <CardContent className="space-y-3 px-4">
          {i.details && <p className="whitespace-pre-wrap text-sm">{i.details}</p>}
          <SentAttachments files={i.files} className="justify-start" />
          <p className="text-muted-foreground text-xs">
            {i.kind === "bug" ? "Bug report" : "Feature request"}
            {i.severity && ` · ${i.severity}`} · sent {new Date(i.created).toLocaleString()} · lyra {i.version}
            {i.page && ` · from ${i.page}`}
          </p>
          {/* lyra's read of it. */}
          {i.analyzing ? (
            <div className="flex items-center gap-2 text-muted-foreground text-xs">
              <Loader2 className="size-3.5 animate-spin" /> lyra is looking at it…
            </div>
          ) : (
            i.analysis && (
              <div className="space-y-2 rounded-lg border border-primary/25 bg-primary/5 p-3 text-sm">
                <div className="flex items-center gap-1.5 font-medium text-primary text-xs uppercase tracking-wide">
                  <Sparkles className="size-3.5" /> {admin ? "lyra's read" : "How lyra understood it"}
                  {i.analysis.area && <span className="font-normal text-muted-foreground normal-case tracking-normal">· {i.analysis.area}</span>}
                  {i.analysis.effort && <Badge variant="outline" className="ml-1 font-normal normal-case tracking-normal">{i.analysis.effort}</Badge>}
                  {admin && (
                    <button type="button" className="ml-auto text-muted-foreground hover:text-foreground" title="Read it again (after replies)" onClick={() => void act("feedback_analyze", {})}>
                      <RefreshCw className="size-3.5" />
                    </button>
                  )}
                </div>
                {i.analysis.error ? (
                  <p className="text-muted-foreground text-xs">Couldn't analyze it: {i.analysis.error}</p>
                ) : (
                  <>
                    <p>{i.analysis.summary}</p>
                    {i.analysis.cause && (
                      <p>
                        <span className="text-muted-foreground">Likely cause: </span>
                        {i.analysis.cause}
                      </p>
                    )}
                    {!!i.analysis.approach?.length && (
                      <div>
                        <div className="text-muted-foreground text-xs">{i.kind === "bug" ? "Possible fixes" : "Possible implementation"}</div>
                        <ol className="list-decimal space-y-0.5 pl-5">
                          {i.analysis.approach.map((a, k) => (
                            <li key={k}>{a}</li>
                          ))}
                        </ol>
                      </div>
                    )}
                    {!!i.analysis.questions?.length && (
                      <div>
                        <div className="text-muted-foreground text-xs">Worth asking</div>
                        <ul className="list-disc space-y-0.5 pl-5">
                          {i.analysis.questions.map((q, k) => (
                            <li key={k}>
                              <button type="button" className="text-left hover:text-primary" title="Ask this in the thread" onClick={() => setText(q)}>
                                {q}
                              </button>
                            </li>
                          ))}
                        </ul>
                      </div>
                    )}
                  </>
                )}
              </div>
            )
          )}
          {/* Admins move it along. */}
          {admin && (
            <div className="space-y-2 rounded-lg border bg-muted/30 p-3">
              <div className="flex flex-wrap items-center gap-2 text-xs">
                <span className="w-14 text-muted-foreground">Status</span>
                <Choice options={Object.entries(STATUS).map(([k, v]) => [k, v.label])} value={i.status} set={(s) => void act("feedback_update", { status: s, shipped_in: s === "done" ? shipped : null })} />
              </div>
              <div className="flex flex-wrap items-center gap-2 text-xs">
                <span className="w-14 text-muted-foreground">Priority</span>
                <Choice options={["low", "normal", "high", "urgent"].map((p) => [p, p])} value={i.priority} set={(p) => void act("feedback_update", { priority: p })} />
              </div>
              <div className="flex flex-wrap items-center gap-2 text-xs">
                <span className="w-14 text-muted-foreground">Shipped</span>
                <Input value={shipped} onChange={(e) => setShipped(e.target.value)} className="h-7 w-32 font-mono text-xs" placeholder={version} />
                <Button size="sm" variant="ghost" className="h-7" onClick={() => void act("feedback_update", { shipped_in: shipped })}>
                  Set
                </Button>
              </div>
            </div>
          )}
          {/* The thread. */}
          <div className="space-y-2">
            {i.comments.map((c, k) =>
              c.system ? (
                <div key={k} className="text-muted-foreground text-xs">
                  {c.name}: {c.text} · {ago(c.at)}
                </div>
              ) : (
                <div key={k} className={cn("rounded-lg px-3 py-2 text-sm", c.by === me ? "ml-8 bg-primary/10" : "mr-8 bg-muted/50")}>
                  <div className="mb-0.5 text-muted-foreground text-xs">
                    {c.name}
                    {c.admin && " (admin)"} · {ago(c.at)}
                  </div>
                  <div className="whitespace-pre-wrap">{c.text}</div>
                </div>
              ),
            )}
            <form
              className="flex gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                if (text.trim()) void act("feedback_comment", { text }).then(() => setText(""));
              }}
            >
              <Input value={text} onChange={(e) => setText(e.target.value)} placeholder={admin && i.user !== me ? `Reply to ${i.name}…` : "Add a comment…"} />
              <Button type="submit" size="sm" variant="secondary" disabled={!text.trim()}>
                Send
              </Button>
            </form>
          </div>
        </CardContent>
      )}
    </Card>
  );
}

export function FeedbackPage({ onBack }: { onBack: () => void }) {
  const { call, ready, status } = useLyra();
  const [data, setData] = useState<Data | null>(null);
  const [filter, setFilter] = useState("open");
  const [kind, setKind] = useState("all");
  const reload = useCallback(() => void call<Data>("feedback").then(setData), [call]);
  // Again when something changes for them (a new one, a reply).
  useEffect(() => {
    if (ready) reload();
  }, [ready, reload, status.feedback_news, status.feedback_rev]);
  const items = (data?.items ?? []).filter((i) => (filter === "all" || (filter === "open" ? !["done", "wontdo"].includes(i.status) : ["done", "wontdo"].includes(i.status))) && (kind === "all" || i.kind === kind));
  return (
    <Page title="Feedback" description="Bug reports and feature requests: send one, and follow where yours stand." action={<Back onBack={onBack} />}>
      <Failed error={data?.error} />
      <SubmitCard sent={reload} page="feedback" />
      <div className="flex flex-wrap items-end justify-between gap-2 pt-2">
        <div>
          <h2 className="font-medium text-base">{data?.admin ? "Everyone's" : "Yours"}</h2>
          <p className="text-muted-foreground text-xs">{data?.admin ? "Move each along; the sender hears about status changes and replies." : "Where each stands; you'll get a note when one moves or someone replies."}</p>
        </div>
        <div className="flex flex-wrap gap-2">
          <Choice
            options={[
              ["all", "All"],
              ["bug", "Bugs"],
              ["feature", "Features"],
            ]}
            value={kind}
            set={setKind}
          />
          <Choice
            options={[
              ["open", "Open"],
              ["closed", "Closed"],
              ["all", "All"],
            ]}
            value={filter}
            set={setFilter}
          />
        </div>
      </div>
      {data && items.length === 0 && <p className="text-muted-foreground text-sm">Nothing here{filter === "open" ? " open" : ""}.</p>}
      {items.map((i) => (
        <ItemCard key={i.id} i={i} admin={!!data?.admin} me={data?.me ?? ""} version={data?.version ?? ""} reload={reload} />
      ))}
    </Page>
  );
}
