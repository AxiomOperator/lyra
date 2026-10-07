// The person's Outlook calendar: connecting it (More), and today's meetings
// (the Tasks page). Changes go through lyra in chat, where anything others
// see waits for the person's yes.

import { Button } from "@/components/ui/button";
import { Card, CardAction, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { CalendarDays, Link2, Unlink, Video } from "lucide-react";
import { useState } from "react";
import { useData, useLyra } from "./store";
import type { CalendarToday, CalEvent } from "./types";

/** Back from connecting (read once, before the app clears the address). */
const fromConnect = (() => {
  const h = new URLSearchParams(location.hash.slice(1));
  const ok = h.get("connected");
  const error = h.get("connect-error");
  if (ok || error) history.replaceState(null, "", location.pathname + location.search);
  return ok ? { ok: true, text: "Your Outlook calendar is connected." } : error ? { ok: false, text: error } : null;
})();

const time = (e: CalEvent) => (e.all_day ? "all day" : (e.start ?? "").split(" ").pop());

/** More: connect, see, or disconnect the calendar. */
export function CalendarCard() {
  const { token, run } = useLyra();
  const [cal, reload] = useData<CalendarToday>("calendar");
  const [note, setNote] = useState(fromConnect);
  if (!cal?.available) return null;
  const connect = async () => {
    setNote(null);
    const r = await fetch("/api/connect/calendar", { method: "POST", headers: { Authorization: "Bearer " + token }, credentials: "same-origin" });
    const b = await r.json().catch(() => ({}));
    if (r.ok && b.url) location.href = b.url;
    else setNote({ ok: false, text: b.error || "Couldn't start connecting." });
  };
  return (
    <Card className="py-4">
      <CardHeader className="px-4">
        <CardTitle className="flex items-center gap-2 text-base">
          <CalendarDays className="size-4" /> Outlook calendar
        </CardTitle>
        <CardDescription>
          {cal.connected
            ? "lyra sees your meetings, finds free time and drafts invites and answers (anything others see waits for your Approve)."
            : "Connect your own calendar: today's meetings in your briefing, free time, and invites drafted for your approval."}
        </CardDescription>
        <CardAction>
          {cal.connected ? (
            <Button
              size="sm"
              variant="ghost"
              onClick={async () => {
                const r = await run("/calendar disconnect");
                setNote({ ok: r.ok, text: r.text });
                reload();
              }}
            >
              <Unlink /> Disconnect
            </Button>
          ) : (
            <Button size="sm" onClick={() => void connect()}>
              <Link2 /> Connect
            </Button>
          )}
        </CardAction>
      </CardHeader>
      {(note || cal.error) && (
        <CardContent className="px-4">
          <p className={note?.ok ? "text-sm text-teal-300" : "text-red-300 text-sm"}>{note?.text ?? cal.error}</p>
        </CardContent>
      )}
    </Card>
  );
}

/** Tasks: today's meetings, clashes and invites waiting. */
export function TodayCard() {
  const [cal] = useData<CalendarToday>("calendar");
  if (!cal?.available || !cal.connected) return null;
  const events = cal.events ?? [];
  return (
    <Card className="gap-0 py-3">
      <CardHeader className="px-4">
        <CardTitle className="flex items-center gap-2 text-sm">
          <CalendarDays className="size-4" /> Today
        </CardTitle>
        <CardAction className="text-muted-foreground text-xs">{events.length ? `${events.length} on the calendar` : ""}</CardAction>
      </CardHeader>
      <CardContent className="space-y-1 px-4 text-sm">
        {cal.error && <p className="text-red-300">{cal.error}</p>}
        {(cal.clashes ?? []).map((c) => (
          <p key={c} className="text-amber-300">
            Double-booked: {c}
          </p>
        ))}
        {events.map((e) => (
          <div key={e.id} className="flex items-baseline gap-3">
            <span className="w-14 shrink-0 text-muted-foreground tabular-nums">{time(e)}</span>
            <span className="min-w-0 flex-1 truncate">
              {e.title}
              {e.where && <span className="text-muted-foreground"> · {e.where}</span>}
              {e.your_answer === "notResponded" && <span className="text-amber-300"> · not answered</span>}
            </span>
            {e.online && <Video className="size-3.5 shrink-0 text-muted-foreground" />}
          </div>
        ))}
        {!cal.error && events.length === 0 && <p className="text-muted-foreground">Nothing on the calendar today.</p>}
        {(cal.invites ?? []).length > 0 && (
          <p className="pt-1 text-amber-300">
            {cal.invites?.length} invite{cal.invites?.length === 1 ? "" : "s"} to answer: ask lyra ("accept the budget review").
          </p>
        )}
      </CardContent>
    </Card>
  );
}
