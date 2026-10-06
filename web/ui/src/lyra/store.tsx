// The live connection to `lyra serve`: a snapshot, then small updates
// (add / append / replace / truncate / reset / status), mirrored into React
// state. Sends messages, approvals, pairing answers and page requests.

import { createContext, useCallback, useContext, useEffect, useMemo, useReducer, useRef, useState, type ReactNode } from "react";
import { saveToken } from "./token";
import type { ChatMessage, Command, Status, ThisDevice } from "./types";

/** The app's own version, stamped into index.html by the server. */
export const APP_VERSION = document.querySelector<HTMLMetaElement>('meta[name="lyra-version"]')?.content ?? "dev";

interface State {
  ready: boolean;
  seq: number;
  messages: ChatMessage[];
  status: Status;
  commands: Command[];
  device: ThisDevice | null;
  serverVersion: string;
}

type Update = { type: string; seq?: number; [k: string]: unknown };

const empty: State = { ready: false, seq: 0, messages: [], status: {}, commands: [], device: null, serverVersion: "" };

function reduce(state: State, msg: Update): State {
  if (msg.type === "snapshot") {
    return {
      ready: true,
      seq: (msg.seq as number) ?? 0,
      messages: (msg.messages as ChatMessage[]) ?? [],
      status: (msg.status as Status) ?? {},
      commands: (msg.commands as Command[]) ?? [],
      device: (msg.device as ThisDevice) ?? null,
      serverVersion: (msg.app_version as string) ?? "",
    };
  }
  if (msg.type === "lost") return { ...state, ready: false };
  if (msg.type === "push") return state.device ? { ...state, device: { ...state.device, push: msg.on as boolean } } : state;
  if (!state.ready) return state;
  if (msg.seq && msg.seq <= state.seq) return state;
  const seq = msg.seq ?? state.seq;
  const index = (msg.index as number) ?? 0;
  switch (msg.type) {
    case "add":
    case "replace": {
      const messages = state.messages.slice();
      messages[index] = msg.message as ChatMessage;
      return { ...state, seq, messages };
    }
    case "append": {
      const m = state.messages[index];
      if (!m) return { ...state, seq };
      const messages = state.messages.slice();
      messages[index] = { ...m, content: m.content + ((msg.text as string) ?? ""), reasoning: (m.reasoning ?? "") + ((msg.reasoning as string) ?? "") };
      return { ...state, seq, messages };
    }
    case "truncate":
      return { ...state, seq, messages: state.messages.slice(0, msg.length as number) };
    case "reset":
      return { ...state, seq, messages: (msg.messages as ChatMessage[]) ?? [] };
    case "status":
      return { ...state, seq, status: (msg.status as Status) ?? {} };
    default:
      return { ...state, seq };
  }
}

type Listener = (what: string, data: unknown) => void;

interface Lyra extends State {
  token: string;
  connected: boolean;
  banner: string;
  send: (obj: object) => boolean;
  say: (text: string) => boolean;
  ask: (what: string, arg?: unknown) => void;
  /** Ask lyra for something and wait for the answer. */
  call: <T = unknown>(what: string, arg?: unknown) => Promise<T>;
  /** Run a page's command (memory, skills, goals, model) and get lyra's answer. */
  run: (command: string) => Promise<{ ok: boolean; text: string }>;
  onData: (fn: Listener) => () => void;
  setPush: (on: boolean) => void;
  unpaired: () => void;
}

const Ctx = createContext<Lyra | null>(null);

export function useLyra(): Lyra {
  const v = useContext(Ctx);
  if (!v) throw new Error("useLyra outside LyraProvider");
  return v;
}

export function LyraProvider({ token, onUnpaired, children }: { token: string; onUnpaired: (why?: string) => void; children: ReactNode }) {
  const [state, dispatch] = useReducer(reduce, empty);
  const [connected, setConnected] = useState(false);
  const [banner, setBanner] = useState("");
  const ws = useRef<WebSocket | null>(null);
  const listeners = useRef(new Set<Listener>());
  const retry = useRef(0);
  const waiting = useRef(new Map<number, (data: unknown) => void>());
  const nextId = useRef(1);

  useEffect(() => {
    let stopped = false;
    let timer: number | undefined;
    const connect = () => {
      const proto = location.protocol === "https:" ? "wss:" : "ws:";
      // Back to the conversation this device had open (the server remembers too).
      let session = "";
      try {
        session = localStorage.getItem("lyra-session") ?? "";
      } catch {
        // no storage: the server's memory of it will do
      }
      const sock = new WebSocket(`${proto}//${location.host}/ws?token=${encodeURIComponent(token)}&session=${encodeURIComponent(session)}`);
      ws.current = sock;
      sock.onopen = () => {
        retry.current = 0;
        setConnected(true);
        setBanner("");
        sock.send(JSON.stringify({ type: "visible", visible: document.visibilityState === "visible" }));
      };
      sock.onmessage = (e) => {
        const msg = JSON.parse(e.data) as Update;
        if (msg.type === "data") {
          const done = typeof msg.id === "number" ? waiting.current.get(msg.id) : undefined;
          if (done) {
            waiting.current.delete(msg.id as number);
            done(msg.data);
          }
          listeners.current.forEach((fn) => fn(msg.what as string, msg.data));
          return;
        }
        if (msg.type === "resync") {
          sock.close();
          return;
        }
        if (msg.type === "snapshot" && typeof msg.session_id === "string") {
          try {
            localStorage.setItem("lyra-session", msg.session_id);
          } catch {
            // fine
          }
        }
        if (msg.type !== "pong") dispatch(msg);
      };
      sock.onclose = () => {
        setConnected(false);
        // Nothing comes back for questions asked on this connection.
        waiting.current.forEach((done) => done({ error: "lost the connection to lyra", ok: false, text: "lost the connection to lyra" }));
        waiting.current.clear();
        dispatch({ type: "lost" });
        if (stopped || ws.current !== sock) return;
        ws.current = null;
        const wait = Math.min(15000, 500 * 2 ** retry.current++);
        fetch("/api/me", { headers: { Authorization: "Bearer " + token } })
          .then((r) => {
            if (r.status === 401) {
              saveToken(null);
              onUnpaired("This device isn't paired any more. Pair it again.");
              return;
            }
            setBanner(`Can't reach lyra — retrying in ${Math.round(wait / 1000)}s`);
            timer = window.setTimeout(connect, wait);
          })
          .catch(() => {
            setBanner(`Offline — retrying in ${Math.round(wait / 1000)}s`);
            timer = window.setTimeout(connect, wait);
          });
      };
    };
    connect();
    // On screen? Then lyra needn't send notifications.
    const visible = () => {
      const s = ws.current;
      if (document.visibilityState === "visible" && !s) connect();
      if (s && s.readyState === 1) s.send(JSON.stringify({ type: "visible", visible: document.visibilityState === "visible" }));
    };
    document.addEventListener("visibilitychange", visible);
    const beat = window.setInterval(() => document.visibilityState === "visible" && visible(), 30000);
    return () => {
      stopped = true;
      window.clearTimeout(timer);
      window.clearInterval(beat);
      document.removeEventListener("visibilitychange", visible);
      ws.current?.close();
    };
  }, [token, onUnpaired]);

  const send = useCallback((obj: object) => {
    const s = ws.current;
    if (s && s.readyState === 1) {
      s.send(JSON.stringify(obj));
      return true;
    }
    setBanner("Not connected — reconnecting…");
    return false;
  }, []);
  const say = useCallback((text: string) => send({ type: "send", text }), [send]);
  const ask = useCallback((what: string, arg?: unknown) => void send({ type: "get", what, arg }), [send]);
  const call = useCallback(
    <T,>(what: string, arg?: unknown) =>
      new Promise<T>((resolve) => {
        const id = nextId.current++;
        waiting.current.set(id, resolve as (data: unknown) => void);
        if (!send({ type: "get", what, arg, id })) {
          waiting.current.delete(id);
          resolve({ error: "not connected", ok: false, text: "not connected to lyra" } as T);
        }
      }),
    [send],
  );
  const run = useCallback((command: string) => call<{ ok: boolean; text: string }>("do", { command }), [call]);
  const onData = useCallback((fn: Listener) => {
    listeners.current.add(fn);
    return () => void listeners.current.delete(fn);
  }, []);
  const setPush = useCallback((on: boolean) => dispatch({ type: "push", on }), []);
  const unpaired = useCallback(() => {
    saveToken(null);
    ws.current?.close();
    onUnpaired();
  }, [onUnpaired]);

  const value = useMemo<Lyra>(
    () => ({ ...state, token, connected, banner, send, say, ask, call, run, onData, setPush, unpaired }),
    [state, token, connected, banner, send, say, ask, call, run, onData, setPush, unpaired],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

/** Ask for a page's data and keep the latest answer. */
export function useData<T>(what: string, deps: unknown[] = []): [T | null, () => void] {
  const { ask, onData, ready } = useLyra();
  const [data, setData] = useState<T | null>(null);
  useEffect(() => onData((w, d) => w === what && setData(d as T)), [onData, what]);
  const refresh = useCallback(() => ask(what), [ask, what]);
  useEffect(() => {
    if (ready) refresh();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [ready, refresh, ...deps]);
  return [data, refresh];
}
