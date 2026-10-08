// What the app keeps on this device for a poor or missing connection:
// messages waiting to be sent (the outbox), what's being written in each
// conversation (drafts), and the last conversation seen (so the app opens
// with it when lyra can't be reached). All in localStorage, all forgotten
// when the device is unpaired. Storage can be blocked: everything here then
// quietly does nothing and the app works as before.

import type { ChatMessage, Status } from "./types";

/** A message written while it couldn't be sent yet. */
export interface Queued {
  id: string;
  /** The conversation it was written in: it's only sent there. */
  session: string;
  text: string;
  at: number;
  /** Sent on this connection, when: it leaves the outbox once the conversation shows it. */
  sentAt?: number;
  gen?: number;
  /** How often the conversation had this text from the user when it was sent. */
  seen?: number;
}

const OUTBOX = "lyra-outbox";
const DRAFT = "lyra-draft:";
const LAST = "lyra-last";

function read<T>(key: string, fallback: T): T {
  try {
    const t = localStorage.getItem(key);
    return t ? (JSON.parse(t) as T) : fallback;
  } catch {
    return fallback;
  }
}

function write(key: string, value: unknown) {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, JSON.stringify(value));
    return true;
  } catch {
    return false;
  }
}

export function loadOutbox(): Queued[] {
  return read<Queued[]>(OUTBOX, []).filter((q) => q && typeof q.text === "string");
}

export function saveOutbox(list: Queued[]) {
  write(OUTBOX, list.length ? list : null);
}

export function newQueued(session: string, text: string): Queued {
  return { id: `${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`, session, text, at: Date.now() };
}

/** How many of the user's messages in a conversation say exactly this. */
export function timesSaid(messages: ChatMessage[], text: string) {
  const t = text.trim();
  return messages.filter((m) => m.role === "user" && m.content.trim() === t).length;
}

// ---- drafts

export function loadDraft(session: string): string {
  return read<string>(DRAFT + session, "");
}

export function saveDraft(session: string, text: string) {
  write(DRAFT + session, text.trim() ? text : null);
}

// ---- the last conversation, for opening without a connection

export interface Last {
  session: string;
  messages: ChatMessage[];
  status: Pick<Status, "session" | "title" | "model" | "context_window">;
}

export function loadLast(): Last | null {
  return read<Last | null>(LAST, null);
}

export function saveLast(messages: ChatMessage[], status: Status) {
  if (!status.session) return;
  const keep = (n: number): Last => ({
    session: status.session as string,
    // Its recent part, without long tool output (that's what makes it big).
    messages: messages.slice(-n).map((m) => (m.role === "tool" || m.role === "agent_tool" ? { ...m, content: m.content.slice(0, 2000) } : m)),
    status: { session: status.session, title: status.title, model: status.model, context_window: status.context_window },
  });
  // Too big for this device's storage: keep less.
  for (const n of [150, 50, 10]) if (write(LAST, keep(n))) return;
}

/** This device was unpaired: nothing of it stays behind. */
export function forgetAll() {
  try {
    for (const k of Object.keys(localStorage)) if (k === OUTBOX || k === LAST || k.startsWith(DRAFT)) localStorage.removeItem(k);
  } catch {
    // nothing to forget
  }
}
