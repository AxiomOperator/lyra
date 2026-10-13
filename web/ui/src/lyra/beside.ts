// Chat beside a page: on a wide screen the chat stays open next to Tasks,
// Notes or a meeting. Each of those pages says what it shows (with ids), and
// a message sent from the chat beside it takes that along, so "turn these
// into tasks" works on what's on screen.

import { useEffect } from "react";

let open = false;
const shown = new Map<string, string>();

/** The chat beside is open (or closed). */
export function setBeside(on: boolean) {
  open = on;
}

/** A page's own words for what it shows now, while it's on screen. */
export function useLooking(page: string, text: string) {
  useEffect(() => {
    shown.set(page, text);
    return () => {
      shown.delete(page);
    };
  }, [page, text]);
}

/** What a message from the chat beside takes along: the pages on screen (nothing when it isn't open). */
export function lookingNow(): string {
  if (!open) return "";
  return [...shown.entries()].map(([page, text]) => `[${page}]\n${text}`).join("\n\n").slice(0, 12000);
}
