// The rail's folding groups a person keeps open, on this device (a phone and
// a laptop have different room). Pages read and change it; the rail listens.

/** The groups that fold (Chat and Status are always single pages). */
export const FOLDING = [
  { id: "work", label: "Work", pages: "Tasks, Meetings, Notes, Documents, Projects" },
  { id: "knowledge", label: "Knowledge", pages: "Memory, Skills" },
  { id: "automation", label: "Automation", pages: "Routines, Goals, Coding" },
  { id: "system", label: "System", pages: "Machines, Devices, Users, Usage, Running now, Model, Activity, Settings" },
  { id: "help", label: "Help", pages: "Q&A, Feedback, What's new" },
];

const KEY = "lyra-rail-open";
const EVENT = "lyra-rail-open";

export function loadOpen(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** Keep a group always open (or not), and tell the rail. */
export function setOpen(id: string, on: boolean) {
  const now = loadOpen().filter((x) => x !== id);
  if (on) now.push(id);
  try {
    localStorage.setItem(KEY, JSON.stringify(now));
  } catch {
    // kept until the page reloads
  }
  window.dispatchEvent(new CustomEvent(EVENT, { detail: now }));
}

/** Follow changes (from the Profile, or the rail's own pin). */
export function onOpenChange(f: (open: string[]) => void): () => void {
  const h = (e: Event) => f((e as CustomEvent<string[]>).detail);
  window.addEventListener(EVENT, h);
  return () => window.removeEventListener(EVENT, h);
}
