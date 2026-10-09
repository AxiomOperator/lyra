// What the app was opened for: a home-screen shortcut (long-press the lyra
// icon: New chat, Add task, Quick note) opens "/?page=…&focus=add" or
// "/?do=new". Read once at start; each page takes what's meant for it.

const params = new URLSearchParams(location.search);
const wanted = new Set<string>();
if (params.get("focus") === "add") wanted.add("add");
if (params.get("do") === "new") wanted.add("new");

/** Whether the app was opened to do this, the first time it's asked (then no more). */
export function takeIntent(what: "add" | "new"): boolean {
  return wanted.delete(what);
}
