// lyra PWA: pairs once, keeps a WebSocket to `lyra serve`, mirrors the
// conversation live, answers approvals, and turns on push notifications.
"use strict";

const $ = (id) => document.getElementById(id);
const state = { token: null, ws: null, seq: 0, messages: [], status: {}, commands: [], device: null, retry: 0, ready: false };

// ---- storage: the token in localStorage (the page) and IndexedDB (the
// service worker answers approvals from notifications with it).

function idb() {
  return new Promise((resolve, reject) => {
    const r = indexedDB.open("lyra", 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}
async function idbSet(key, value) {
  try {
    const db = await idb();
    const tx = db.transaction("kv", "readwrite");
    if (value === null) tx.objectStore("kv").delete(key); else tx.objectStore("kv").put(value, key);
  } catch (e) { /* private mode: notifications' buttons just open the app */ }
}
function saveToken(t) {
  state.token = t;
  try { t ? localStorage.setItem("lyra-token", t) : localStorage.removeItem("lyra-token"); } catch (e) {}
  idbSet("token", t);
}

// ---- Markdown (escaped first, so nothing the model writes becomes HTML)

function esc(s) {
  return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}
function inline(s) {
  const codes = [];
  s = s.replace(/`([^`]+)`/g, (_, c) => { codes.push(c); return "\u0000" + (codes.length - 1) + "\u0000"; });
  s = esc(s);
  s = s.replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, (_, t, u) => `<a href="${u}" target="_blank" rel="noopener">${t}</a>`);
  s = s.replace(/\*\*([^*]+)\*\*/g, "<strong>$1</strong>").replace(/__([^_]+)__/g, "<strong>$1</strong>");
  s = s.replace(/(^|[^*])\*([^*\s][^*]*)\*/g, "$1<em>$2</em>").replace(/(^|\W)_([^_\s][^_]*)_(?=\W|$)/g, "$1<em>$2</em>");
  s = s.replace(/~~([^~]+)~~/g, "<del>$1</del>");
  return s.replace(/\u0000(\d+)\u0000/g, (_, i) => `<code>${esc(codes[+i])}</code>`);
}
function markdown(text) {
  const lines = text.replace(/\r/g, "").split("\n");
  const out = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    const fence = line.match(/^\s*```(\S*)/);
    if (fence) {
      const body = [];
      i++;
      while (i < lines.length && !/^\s*```/.test(lines[i])) body.push(lines[i++]);
      i++;
      out.push(`<pre><code>${esc(body.join("\n"))}</code></pre>`);
      continue;
    }
    if (/^\s*$/.test(line)) { i++; continue; }
    const h = line.match(/^(#{1,6})\s+(.*)/);
    if (h) { const n = Math.min(h[1].length, 4); out.push(`<h${n}>${inline(h[2])}</h${n}>`); i++; continue; }
    if (/^\s*([-*_])\s*\1\s*\1[\s\1]*$/.test(line)) { out.push("<hr>"); i++; continue; }
    if (/^\s*>/.test(line)) {
      const body = [];
      while (i < lines.length && /^\s*>/.test(lines[i])) body.push(lines[i++].replace(/^\s*>\s?/, ""));
      out.push(`<blockquote>${markdown(body.join("\n"))}</blockquote>`);
      continue;
    }
    if (/^\s*\|.*\|\s*$/.test(line) && i + 1 < lines.length && /^\s*\|?[\s:|-]+\|?\s*$/.test(lines[i + 1])) {
      const cells = (l) => l.trim().replace(/^\||\|$/g, "").split("|").map((c) => inline(c.trim()));
      const head = cells(line);
      i += 2;
      const rows = [];
      while (i < lines.length && /^\s*\|.*\|\s*$/.test(lines[i])) rows.push(cells(lines[i++]));
      out.push(`<table><tr>${head.map((c) => `<th>${c}</th>`).join("")}</tr>${rows.map((r) => `<tr>${r.map((c) => `<td>${c}</td>`).join("")}</tr>`).join("")}</table>`);
      continue;
    }
    const item = /^(\s*)([-*+]|\d+[.)])\s+(.*)/;
    if (item.test(line)) {
      const ordered = /\d/.test(line.match(item)[2]);
      const items = [];
      while (i < lines.length && (item.test(lines[i]) || (/^\s{2,}\S/.test(lines[i]) && items.length))) {
        const m = lines[i].match(item);
        if (m) {
          let t = m[3];
          const task = t.match(/^\[([ xX])\]\s+(.*)/);
          if (task) t = (task[1] === " " ? "☐ " : "☑ ") + task[2];
          items.push({ indent: m[1].length, text: t });
        } else items[items.length - 1].text += " " + lines[i].trim();
        i++;
      }
      const tag = ordered ? "ol" : "ul";
      out.push(`<${tag}>${items.map((it) => `<li${it.indent >= 2 ? ' style="margin-left:1em"' : ""}>${inline(it.text)}</li>`).join("")}</${tag}>`);
      continue;
    }
    const para = [];
    while (i < lines.length && !/^\s*$/.test(lines[i]) && !/^\s*(```|#{1,6}\s|>|[-*+]\s|\d+[.)]\s|\|)/.test(lines[i])) para.push(lines[i++]);
    if (!para.length) para.push(lines[i++]);
    out.push(`<p>${para.map(inline).join("<br>")}</p>`);
  }
  return out.join("");
}

// ---- rendering

const LABELS = { user: "you", assistant: "lyra", info: "system", error: "error", agent: "↪ agent", approval: "approval" };

function renderMessage(m, el) {
  el.className = "msg " + m.role;
  if (m.role === "tool") {
    el.innerHTML = `<div class="body mono">↳ ${esc(m.content.slice(0, 300))}</div>`;
    return;
  }
  let html = m.role === "user" ? "" : `<div class="who">${LABELS[m.role] || m.role}</div>`;
  if (m.reasoning && m.reasoning.trim()) {
    html += `<details class="reasoning-wrap"${m.content ? "" : " open"}><summary>thinking</summary><div class="reasoning">${esc(m.reasoning.trim())}</div></details>`;
  }
  const body = m.role === "assistant" || m.role === "agent" ? markdown(m.content) : esc(m.content);
  if (m.content) html += `<div class="body">${body}</div>`;
  if (m.tools && m.tools.length) html += `<div class="calls">${m.tools.map((t) => "→ " + esc(t)).join("<br>")}</div>`;
  const foot = [];
  if (m.stats) foot.push(esc(m.stats));
  if (m.agents && m.agents.length) foot.push(`<span class="agents">handled with: ${esc(m.agents.join(", "))}</span>`);
  if (m.skills && m.skills.length) foot.push(`<span class="skills">used skills: ${esc(m.skills.join(", "))}</span>`);
  if (foot.length) html += `<div class="foot">${foot.join("<br>")}</div>`;
  el.innerHTML = html;
}

function nearBottom() {
  const c = $("chat");
  return c.scrollHeight - c.scrollTop - c.clientHeight < 120;
}
function toBottom() {
  const c = $("chat");
  c.scrollTop = c.scrollHeight;
}

function renderAll() {
  const chat = $("chat");
  chat.innerHTML = "";
  state.messages.forEach((m) => {
    const el = document.createElement("div");
    renderMessage(m, el);
    chat.appendChild(el);
  });
  renderThinking();
  toBottom();
}

function renderThinking() {
  let t = $("thinking");
  const last = state.messages[state.messages.length - 1];
  const show = state.status.waiting && last && ["user", "tool", "agent", "approval"].includes(last.role);
  if (show && !t) {
    t = document.createElement("div");
    t.id = "thinking";
    t.className = "thinking";
    t.textContent = "thinking…";
  }
  if (show) $("chat").appendChild(t);
  else if (t) t.remove();
}

function setMessage(i, m) {
  const chat = $("chat");
  const stick = nearBottom();
  state.messages[i] = m;
  let el = chat.children[i];
  if (!el || el.id === "thinking") {
    el = document.createElement("div");
    chat.insertBefore(el, $("thinking"));
  }
  renderMessage(m, el);
  renderThinking();
  if (stick) toBottom();
}

function renderStatus() {
  const s = state.status;
  const phase = $("phase");
  phase.textContent = s.phase || "";
  phase.className = "phase" + (s.approvals && s.approvals.length ? " asking" : (s.phase || "").startsWith("↪") ? " agent" : s.waiting ? " busy" : "");
  $("session-title").textContent = s.title ? "· " + s.title : "";
  renderApproval();
  renderThinking();
}

function renderApproval() {
  const box = $("approval");
  const list = state.status.approvals || [];
  const a = list[0];
  $("composer").classList.toggle("approval-open", !!a);
  if (!a) { box.classList.add("hidden"); box.innerHTML = ""; return; }
  const stick = nearBottom();
  box.className = "approval-card" + (a.dangerous ? " dangerous" : "");
  box.innerHTML = `
    <div class="head"><span class="agent">${esc(a.agent)}</span> wants to ${esc(a.what)}:</div>
    <div class="detail mono">${esc(a.detail)}</div>
    <div class="why">${a.dangerous ? "Risk: " : "Why it asks: "}${esc(a.why)}</div>
    <div class="buttons">
      <button class="allow" data-answer="y">Allow once</button>
      <button class="deny" data-answer="n">Deny</button>
      <button class="always" data-answer="a">Allow for session</button>
    </div>
    ${list.length > 1 ? `<div class="more">1 of ${list.length} waiting</div>` : ""}`;
  box.querySelectorAll("button").forEach((b) => b.addEventListener("click", () => {
    send({ type: "approve", id: a.id, answer: b.dataset.answer });
    box.querySelectorAll("button").forEach((x) => (x.disabled = true));
  }));
  if (stick) toBottom();
}

// ---- the connection

function send(obj) {
  if (state.ws && state.ws.readyState === 1) { state.ws.send(JSON.stringify(obj)); return true; }
  banner("Not connected — reconnecting…");
  return false;
}

function banner(text) {
  const b = $("banner");
  b.textContent = text || "";
  b.classList.toggle("hidden", !text);
}

function connect() {
  if (state.ws && state.ws.readyState <= 1) return;
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(`${proto}//${location.host}/ws?token=${encodeURIComponent(state.token)}`);
  state.ws = ws;
  state.ready = false;
  ws.onopen = () => { state.retry = 0; $("conn-dot").className = "dot on"; banner(""); reportVisible(); };
  ws.onmessage = (e) => handle(JSON.parse(e.data));
  ws.onclose = (e) => {
    $("conn-dot").className = "dot off";
    if (state.ws !== ws) return;
    state.ws = null;
    fetch("/api/me", { headers: { Authorization: "Bearer " + state.token } }).then((r) => {
      if (r.status === 401) { saveToken(null); showPair("This device isn't paired any more. Pair it again."); return; }
      const wait = Math.min(15000, 500 * 2 ** state.retry++);
      banner(`Can't reach lyra — retrying in ${Math.round(wait / 1000)}s`);
      setTimeout(connect, wait);
    }).catch(() => {
      const wait = Math.min(15000, 500 * 2 ** state.retry++);
      banner(`Offline — retrying in ${Math.round(wait / 1000)}s`);
      setTimeout(connect, wait);
    });
  };
}

function handle(msg) {
  if (msg.type === "snapshot") {
    state.seq = msg.seq || 0;
    state.messages = msg.messages || [];
    state.status = msg.status || {};
    state.commands = msg.commands || [];
    state.device = msg.device || null;
    state.ready = true;
    renderAll();
    renderStatus();
    updateNotifyButton();
    openApprovalFromUrl();
    return;
  }
  if (msg.type === "resync") { state.ws && state.ws.close(); return; }
  if (msg.type === "pong" || !state.ready) return;
  if (msg.seq && msg.seq <= state.seq) return;
  if (msg.seq) state.seq = msg.seq;
  switch (msg.type) {
    case "add":
    case "replace":
      setMessage(msg.index, msg.message);
      break;
    case "append": {
      const m = state.messages[msg.index];
      if (!m) break;
      m.content += msg.text || "";
      m.reasoning = (m.reasoning || "") + (msg.reasoning || "");
      setMessage(msg.index, m);
      break;
    }
    case "truncate":
      state.messages.length = msg.length;
      renderAll();
      break;
    case "reset":
      state.messages = msg.messages || [];
      renderAll();
      break;
    case "status":
      state.status = msg.status || {};
      renderStatus();
      break;
  }
}

// Visible on screen? Then lyra doesn't need to send a notification.
function reportVisible() {
  send({ type: "visible", visible: document.visibilityState === "visible" });
}
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible" && (!state.ws || state.ws.readyState > 1) && state.token) connect();
  if (state.ws && state.ws.readyState === 1) reportVisible();
});
setInterval(() => { if (document.visibilityState === "visible") reportVisible(); }, 30000);

// ---- composing

function autosize() {
  const t = $("input");
  t.style.height = "auto";
  t.style.height = Math.min(t.scrollHeight, window.innerHeight * 0.4) + "px";
}

function renderPalette() {
  const text = $("input").value;
  const p = $("palette");
  if (!text.startsWith("/") || !state.commands.length) { p.classList.add("hidden"); return; }
  const q = text.toLowerCase();
  const word = q.split(/\s+/)[0];
  let found = state.commands.filter((c) => c.usage.toLowerCase().startsWith(q));
  if (!found.length && q.includes(" ")) found = state.commands.filter((c) => c.usage.split(/\s+/)[0] === word);
  if (!found.length) { p.classList.add("hidden"); return; }
  p.innerHTML = found.slice(0, 40).map((c, i) => `<div data-i="${i}"><div class="usage">${esc(c.usage)}</div>${c.description ? `<div class="desc">${esc(c.description)}</div>` : ""}</div>`).join("");
  p.querySelectorAll("div[data-i]").forEach((el) => el.addEventListener("click", () => {
    const c = found[+el.dataset.i];
    $("input").value = c.completion;
    p.classList.add("hidden");
    $("input").focus();
    if (!c.completion.endsWith(" ")) submit();
  }));
  p.classList.remove("hidden");
}

function submit() {
  const t = $("input");
  const text = t.value.trim();
  if (!text) return;
  if (send({ type: "send", text })) {
    t.value = "";
    autosize();
    $("palette").classList.add("hidden");
    toBottom();
  }
}

$("composer").addEventListener("submit", (e) => { e.preventDefault(); submit(); });
$("input").addEventListener("input", () => { autosize(); renderPalette(); });
$("input").addEventListener("keydown", (e) => {
  // Desktop: Enter sends, Shift-Enter is a new line. Touch keyboards keep Enter for new lines.
  if (e.key === "Enter" && !e.shiftKey && !("ontouchstart" in window)) { e.preventDefault(); submit(); }
});

// ---- menu: sessions, notifications, unpair

function openMenu() {
  $("menu").classList.remove("hidden");
  $("menu-device").textContent = state.device ? `This device: ${state.device.name}` : "";
  const machines = state.status.machines || [];
  $("menu-machines").textContent = machines.length ? `Machines lyra can work on: server, ${machines.join(", ")}` : "Machines: server only (run `lyra node` on a PC to add it)";
  updateNotifyButton();
}
$("menu-button").addEventListener("click", openMenu);
$("menu").addEventListener("click", (e) => { if (e.target === $("menu") || e.target.dataset.close !== undefined) $("menu").classList.add("hidden"); });
$("new-chat").addEventListener("click", () => { send({ type: "send", text: "/new" }); $("menu").classList.add("hidden"); });
$("sessions-button").addEventListener("click", () => send({ type: "send", text: "/sessions" }) && ($("menu").classList.add("hidden")));
$("unpair").addEventListener("click", async () => {
  if (!confirm("Unpair this device? You'll need a new code from `lyra pair` to use it again.")) return;
  await disableNotifications();
  saveToken(null);
  state.ws && state.ws.close();
  showPair();
});

function isIos() { return /iphone|ipad|ipod/i.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1); }
function standalone() { return window.matchMedia("(display-mode: standalone)").matches || navigator.standalone === true; }

function updateNotifyButton() {
  const b = $("notify-button");
  const note = $("notify-note");
  const test = $("test-notify");
  note.textContent = "";
  if (!("serviceWorker" in navigator) || !window.isSecureContext) {
    b.disabled = true; b.textContent = "Notifications need HTTPS";
    note.textContent = "Open lyra through its https:// address (your reverse proxy) to get notifications.";
    test.classList.add("hidden");
    return;
  }
  if (isIos() && !standalone()) {
    b.disabled = true; b.textContent = "Add to Home Screen first";
    note.textContent = "On iPhone, notifications work once lyra is on the Home Screen: Share → Add to Home Screen, then open it from there.";
    test.classList.add("hidden");
    return;
  }
  if (!("PushManager" in window)) { b.disabled = true; b.textContent = "This browser can't receive notifications"; return; }
  b.disabled = false;
  const on = state.device && state.device.push && Notification.permission === "granted";
  b.textContent = on ? "Turn off notifications" : "Turn on notifications";
  test.classList.toggle("hidden", !on);
  if (Notification.permission === "denied") note.textContent = "Notifications are blocked for this site in your browser or phone settings.";
}

function b64ToBytes(s) {
  const pad = "=".repeat((4 - (s.length % 4)) % 4);
  const raw = atob((s + pad).replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}

async function enableNotifications() {
  const permission = await Notification.requestPermission();
  if (permission !== "granted") { updateNotifyButton(); return; }
  const reg = await navigator.serviceWorker.ready;
  const { key } = await (await fetch("/api/vapid")).json();
  let sub = await reg.pushManager.getSubscription();
  if (sub) await sub.unsubscribe();
  sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64ToBytes(key) });
  const r = await fetch("/api/push", { method: "POST", headers: { "Content-Type": "application/json", Authorization: "Bearer " + state.token }, body: JSON.stringify({ subscription: sub.toJSON() }) });
  if (!r.ok) throw new Error((await r.json()).error || r.statusText);
  if (state.device) state.device.push = true;
  updateNotifyButton();
}

async function disableNotifications() {
  try {
    const reg = await navigator.serviceWorker.ready;
    const sub = await reg.pushManager.getSubscription();
    if (sub) await sub.unsubscribe();
    await fetch("/api/push", { method: "POST", headers: { "Content-Type": "application/json", Authorization: "Bearer " + state.token }, body: JSON.stringify({ subscription: null }) });
  } catch (e) {}
  if (state.device) state.device.push = false;
  updateNotifyButton();
}

$("notify-button").addEventListener("click", async () => {
  try {
    if (state.device && state.device.push && Notification.permission === "granted") await disableNotifications();
    else await enableNotifications();
  } catch (e) { $("notify-note").textContent = "Couldn't turn notifications on: " + e.message; }
});
$("test-notify").addEventListener("click", async () => {
  const r = await fetch("/api/test-push", { method: "POST", headers: { Authorization: "Bearer " + state.token } });
  $("notify-note").textContent = r.ok ? "Sent — it should arrive in a few seconds." : "Failed: " + ((await r.json()).error || r.statusText);
});

// A notification tapped on iPhone (no buttons there) opens lyra here.
function openApprovalFromUrl() {
  const p = new URLSearchParams(location.search);
  if (p.has("approval")) history.replaceState(null, "", "/");
}

// ---- pairing

function showPair(message) {
  $("app").classList.add("hidden");
  $("pair").classList.remove("hidden");
  $("pair-error").textContent = message || "";
  const ua = navigator.userAgent;
  $("pair-name").value = /iphone/i.test(ua) ? "iPhone" : /ipad/i.test(ua) ? "iPad" : /android/i.test(ua) ? "Android" : "Browser";
  $("pair-code").focus();
}

$("pair-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  $("pair-error").textContent = "";
  const r = await fetch("/api/pair", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ code: $("pair-code").value, name: $("pair-name").value }) });
  const body = await r.json().catch(() => ({}));
  if (!r.ok) { $("pair-error").textContent = body.error || "Pairing failed"; return; }
  saveToken(body.token);
  start();
});

function start() {
  $("pair").classList.add("hidden");
  $("app").classList.remove("hidden");
  connect();
}

// ---- boot

if ("serviceWorker" in navigator && window.isSecureContext) navigator.serviceWorker.register("/sw.js").catch(() => {});
try { state.token = localStorage.getItem("lyra-token"); } catch (e) {}
if (state.token) { idbSet("token", state.token); start(); } else showPair();
