// Project folders: folders on this PC the person lends lyra (the browser's
// File System Access, Chrome/Edge). The handles stay in this browser
// (IndexedDB); lyra's requests for them come over the WebSocket, only ever
// from this person's own conversations, and a write only after their yes.

// The parts of the File System Access API used here (not in TypeScript's DOM lib yet).
type Mode = { mode: "read" | "readwrite" };
interface Entry {
  kind: "file" | "directory";
  name: string;
}
interface Dir extends Entry {
  kind: "directory";
  values(): AsyncIterable<Dir | FileH>;
  getDirectoryHandle(name: string, o?: { create?: boolean }): Promise<Dir>;
  getFileHandle(name: string, o?: { create?: boolean }): Promise<FileH>;
  queryPermission(m: Mode): Promise<PermissionState>;
  requestPermission(m: Mode): Promise<PermissionState>;
}
interface Writable {
  seek(at: number): Promise<void>;
  write(data: string): Promise<void>;
  close(): Promise<void>;
}
interface FileH extends Entry {
  kind: "file";
  getFile(): Promise<File>;
  createWritable(o?: { keepExistingData?: boolean }): Promise<Writable>;
}

export interface ProjectFolder {
  name: string;
  handle: Dir;
  /** lyra may change files here without asking each time. */
  trusted?: boolean;
}
export interface FolderState {
  name: string;
  writable: boolean;
  allowed: boolean;
  trusted: boolean;
}

export const supported = typeof window !== "undefined" && "showDirectoryPicker" in window;

const SKIP = new Set([".git", "node_modules", "target", "dist", ".venv", "__pycache__", ".next", "build"]);

function idb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const r = indexedDB.open("lyra", 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}

async function load(): Promise<ProjectFolder[]> {
  try {
    const db = await idb();
    return await new Promise((resolve) => {
      const r = db.transaction("kv").objectStore("kv").get("folders");
      r.onsuccess = () => resolve((r.result as ProjectFolder[] | undefined) ?? []);
      r.onerror = () => resolve([]);
    });
  } catch {
    return [];
  }
}

async function save(list: ProjectFolder[]) {
  const db = await idb();
  await new Promise<void>((resolve, reject) => {
    const tx = db.transaction("kv", "readwrite");
    tx.objectStore("kv").put(list, "folders");
    tx.oncomplete = () => resolve();
    tx.onerror = () => reject(tx.error);
  });
  changed();
}

// Who hears about changes: the connection (tells lyra) and the Projects page.
const listeners = new Set<() => void>();
export function onFoldersChanged(fn: () => void) {
  listeners.add(fn);
  return () => void listeners.delete(fn);
}
function changed() {
  listeners.forEach((fn) => fn());
}

export async function folderStates(): Promise<FolderState[]> {
  const list = await load();
  return Promise.all(
    list.map(async (f) => {
      const rw = await f.handle.queryPermission({ mode: "readwrite" }).catch(() => "denied" as PermissionState);
      const r = rw === "granted" ? rw : await f.handle.queryPermission({ mode: "read" }).catch(() => "denied" as PermissionState);
      return { name: f.name, writable: rw === "granted", allowed: r === "granted", trusted: !!f.trusted };
    }),
  );
}

/** Pick a folder on this PC (needs a tap). */
export async function addFolder(): Promise<string | null> {
  const pick = (window as unknown as { showDirectoryPicker: (o: Mode & { id?: string }) => Promise<Dir> }).showDirectoryPicker;
  let handle: Dir;
  try {
    handle = await pick({ mode: "readwrite", id: "lyra-project" });
  } catch {
    return null; // cancelled
  }
  const list = await load();
  let name = handle.name;
  for (let i = 2; list.some((f) => f.name.toLowerCase() === name.toLowerCase()); i++) name = `${handle.name} ${i}`;
  await save([...list, { name, handle }]);
  return name;
}

export async function removeFolder(name: string) {
  await save((await load()).filter((f) => f.name !== name));
}

export async function renameFolder(name: string, to: string) {
  const list = await load();
  const t = to.trim();
  if (!t || list.some((f) => f.name.toLowerCase() === t.toLowerCase() && f.name !== name)) return false;
  await save(list.map((f) => (f.name === name ? { ...f, name: t } : f)));
  return true;
}

/** Let lyra change files in a folder without asking each time (or ask again). */
export async function trustFolder(name: string, on: boolean) {
  await save((await load()).map((f) => (f.name === name ? { ...f, trusted: on } : f)));
}

/** Give lyra the browser's OK again (after a restart it may ask; needs a tap). */
export async function allowFolder(name: string) {
  const f = (await load()).find((x) => x.name === name);
  if (f) await f.handle.requestPermission({ mode: "readwrite" }).catch(() => "denied");
  changed();
}

function parts(path: string): string[] {
  const p = (path ?? "").replace(/\\/g, "/").split("/").filter((x) => x && x !== ".");
  if (p.includes("..") || /^[a-z]:/i.test(path ?? "") || (path ?? "").startsWith("/")) throw new Error("that path is outside the folder");
  return p;
}

async function dirAt(root: Dir, p: string[], create = false): Promise<Dir> {
  let d = root;
  for (const name of p) d = await d.getDirectoryHandle(name, { create });
  return d;
}

async function fileAt(root: Dir, path: string, create = false): Promise<FileH> {
  const p = parts(path);
  const name = p.pop();
  if (!name) throw new Error("which file?");
  return (await dirAt(root, p, create)).getFileHandle(name, { create });
}

function looksText(bytes: Uint8Array) {
  return !bytes.subarray(0, 8000).includes(0);
}

function base64(buf: ArrayBuffer) {
  const bytes = new Uint8Array(buf);
  let s = "";
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

type Req = { op: string; folder: string; path?: string; depth?: number; query?: string; content?: string; append?: boolean; binary?: boolean; max_bytes?: number };

/** Answer one of lyra's requests for a folder. */
export async function answer(req: Req): Promise<{ result?: unknown; error?: string }> {
  try {
    const f = (await load()).find((x) => x.name.toLowerCase() === (req.folder ?? "").toLowerCase());
    if (!f) return { error: `this page doesn't have the folder ${req.folder}` };
    const need = req.op === "write" ? "readwrite" : "read";
    if ((await f.handle.queryPermission({ mode: need })) !== "granted") return { error: `lyra needs your OK for ${f.name} again: open Projects in lyra and tap Allow` };
    const root = f.handle;
    switch (req.op) {
      case "list": {
        const out: { path: string; dir?: boolean; size?: number }[] = [];
        const walk = async (d: Dir, at: string, depth: number) => {
          for await (const e of d.values()) {
            if (out.length >= 500) return;
            const path = at ? `${at}/${e.name}` : e.name;
            if (e.kind === "directory") {
              out.push({ path, dir: true });
              if (depth > 1 && !SKIP.has(e.name)) await walk(e, path, depth - 1);
            } else {
              out.push({ path, size: (await e.getFile()).size });
            }
          }
        };
        const p = parts(req.path ?? "");
        await walk(await dirAt(root, p), p.join("/"), Math.min(4, Math.max(1, req.depth ?? 1)));
        out.sort((a, b) => a.path.localeCompare(b.path));
        return { result: { folder: f.name, entries: out, cut: out.length >= 500 } };
      }
      case "read": {
        const file = await (await fileAt(root, req.path ?? "")).getFile();
        const max = req.max_bytes ?? 400_000;
        if (req.binary) {
          if (file.size > max) return { error: `${file.name} is too big to read here (${Math.round(file.size / 1e6)} MB)` };
          return { result: { base64: base64(await file.arrayBuffer()) } };
        }
        const bytes = new Uint8Array(await file.slice(0, max).arrayBuffer());
        if (!looksText(bytes)) return { result: { path: req.path, size: file.size, type: file.type || "binary", note: "not a text file lyra can read" } };
        return { result: { text: new TextDecoder().decode(bytes), cut: file.size > max } };
      }
      case "search": {
        const q = (req.query ?? "").toLowerCase();
        const hits: { path: string; line?: number; text?: string }[] = [];
        let seen = 0;
        const walk = async (d: Dir, at: string) => {
          for await (const e of d.values()) {
            if (hits.length >= 60 || seen >= 3000) return;
            const path = at ? `${at}/${e.name}` : e.name;
            if (e.kind === "directory") {
              if (!SKIP.has(e.name)) await walk(e, path);
              continue;
            }
            seen++;
            if (e.name.toLowerCase().includes(q)) hits.push({ path });
            const file = await e.getFile();
            if (file.size > 1_000_000) continue;
            const bytes = new Uint8Array(await file.arrayBuffer());
            if (!looksText(bytes)) continue;
            const lines = new TextDecoder().decode(bytes).split("\n");
            for (let i = 0; i < lines.length && hits.length < 60; i++) {
              if (lines[i].toLowerCase().includes(q)) hits.push({ path, line: i + 1, text: lines[i].trim().slice(0, 200) });
            }
          }
        };
        const p = parts(req.path ?? "");
        await walk(await dirAt(root, p), p.join("/"));
        return { result: { folder: f.name, hits, files_looked_at: seen, cut: hits.length >= 60 || seen >= 3000 } };
      }
      case "write": {
        const h = await fileAt(root, req.path ?? "", true);
        const w = await h.createWritable({ keepExistingData: !!req.append });
        if (req.append) await w.seek((await h.getFile()).size);
        await w.write(req.content ?? "");
        await w.close();
        return { result: { wrote: req.path, folder: f.name, appended: !!req.append, chars: (req.content ?? "").length } };
      }
      default:
        return { error: `can't ${req.op} here` };
    }
  } catch (e) {
    const m = e instanceof Error ? e.message : String(e);
    return { error: /NotFound|could not be found/i.test(m) ? `${req.path} isn't in ${req.folder}` : m };
  }
}
