// lyra PWA: pairs once, keeps a WebSocket to `lyra serve`, mirrors the
// conversation live, answers approvals, and turns on push notifications.
"use strict";

const $ = (id) => document.getElementById(id);
// Stamped by the server; when it says it has another version, offer the update.
const LYRA_VERSION = "__LYRA_VERSION__";
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
  renderPairing();
  renderThinking();
  renderBadges();
  if (state.page === "machines") renderMachines();
  // The devices page follows who's online (not every status tick).
  const who = JSON.stringify([s.online || [], (s.machines_detail || []).map((m) => [m.name, m.online])]);
  if (state.page === "devices" && who !== state.lastWho) ask("devices");
  state.lastWho = who;
}

// Headless machines asking to pair: approve from here (check the code matches).
function pairCard(p) {
  return `<div class="pair-request">
    <div><strong>${esc(p.name)}</strong> <span class="muted">(${esc(p.hostname || "?")}${p.os ? ", " + esc(p.os) : ""})</span> asks to pair as a ${p.kind === "node" ? "machine" : "device"}.</div>
    <div class="muted">Check the machine shows this code: <span class="code">${esc(p.code)}</span></div>
    <div class="actions row"><button class="allow" data-pair="${esc(p.id)}" data-approve="1">Approve</button><button class="deny" data-pair="${esc(p.id)}" data-approve="0">Deny</button></div>
  </div>`;
}
function bindPairButtons(root) {
  root.querySelectorAll("button[data-pair]").forEach((b) => b.addEventListener("click", () => {
    send({ type: "pair_answer", id: b.dataset.pair, approve: b.dataset.approve === "1" });
    b.closest(".pair-request").querySelectorAll("button").forEach((x) => (x.disabled = true));
  }));
}
function renderPairing() {
  const list = state.status.pairing || [];
  for (const id of ["pairing-cards", "machine-pairing", "device-pairing"]) {
    const el = $(id);
    el.innerHTML = list.map(pairCard).join("");
    bindPairButtons(el);
  }
}
function renderBadges() {
  const pairing = (state.status.pairing || []).length;
  const updates = (state.status.machines_detail || []).filter((m) => m.update_available).length;
  const bm = $("badge-machines");
  bm.textContent = pairing + updates || "";
  bm.classList.toggle("hidden", !(pairing + updates));
  const bd = $("badge-devices");
  bd.textContent = pairing || "";
  bd.classList.toggle("hidden", !pairing);
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
    state.serverVersion = msg.app_version || "";
    $("update-banner").classList.toggle("hidden", !state.serverVersion || state.serverVersion === LYRA_VERSION);
    renderAll();
    renderStatus();
    updateNotifyButton();
    openApprovalFromUrl();
    return;
  }
  if (msg.type === "resync") { state.ws && state.ws.close(); return; }
  if (msg.type === "data") { showData(msg.what, msg.data); return; }
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

// `@` + the start of a machine's name: pick where the Operator should work.
function mentionEntries(text) {
  const word = text.split(/\s/).pop();
  if (!word.startsWith("@")) return null;
  const typed = word.slice(1).toLowerCase();
  const before = text.slice(0, text.length - word.length);
  const entries = [{ usage: "@server", description: "where lyra runs", completion: before + "@server " }];
  for (const m of state.status.machines_detail || []) {
    if (!m.online) continue;
    entries.push({ usage: "@" + m.name, description: ["online", m.hostname, m.os].filter(Boolean).join(" · "), completion: before + "@" + m.name + " " });
  }
  return entries.filter((e) => e.usage.slice(1).toLowerCase().startsWith(typed));
}

function renderPalette() {
  const text = $("input").value;
  const p = $("palette");
  const mentions = mentionEntries(text);
  if (mentions) {
    showPalette(mentions);
    return;
  }
  if (!text.startsWith("/") || !state.commands.length) { p.classList.add("hidden"); return; }
  const q = text.toLowerCase();
  const word = q.split(/\s+/)[0];
  let found = state.commands.filter((c) => c.usage.toLowerCase().startsWith(q));
  if (!found.length && q.includes(" ")) found = state.commands.filter((c) => c.usage.split(/\s+/)[0] === word);
  if (!found.length) { p.classList.add("hidden"); return; }
  showPalette(found);
}

function showPalette(found) {
  const p = $("palette");
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
  if (e.key === "Enter" && !e.shiftKey && !("ontouchstart" in window)) {
    e.preventDefault();
    // An open @ list: Enter picks the first machine rather than sending.
    const mentions = mentionEntries($("input").value);
    if (mentions && mentions.length) {
      $("input").value = mentions[0].completion;
      $("palette").classList.add("hidden");
      return;
    }
    submit();
  }
});

// ---- pages: machines, devices, activity, more

function ask(what) {
  send({ type: "get", what });
}

function showPage(page) {
  state.page = page;
  document.querySelectorAll(".page").forEach((el) => el.classList.toggle("hidden", el.id !== "page-" + page));
  document.querySelectorAll("#tabs button").forEach((b) => b.classList.toggle("active", b.dataset.page === page));
  if (page === "chat") toBottom();
  if (page === "machines") renderMachines();
  if (page === "devices") ask("devices");
  if (page === "activity") ask("activity");
  if (page === "more") {
    ask("sessions");
    ask("about");
    $("menu-device").textContent = state.device ? `This device: ${state.device.name}` : "";
    updateNotifyButton();
  }
}
document.querySelectorAll("#tabs button").forEach((b) => b.addEventListener("click", () => showPage(b.dataset.page)));

function ago(iso) {
  if (!iso) return "";
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 90) return "just now";
  if (s < 3600) return Math.round(s / 60) + " min ago";
  if (s < 86400) return Math.round(s / 3600) + " h ago";
  return Math.round(s / 86400) + " days ago";
}

function command(text, confirmText) {
  if (confirmText && !confirm(confirmText)) return;
  send({ type: "send", text });
}

function renderMachines() {
  const list = state.status.machines_detail || [];
  const el = $("machine-list");
  el.innerHTML = [
    `<div class="card"><div class="top"><span class="dot on"></span><span class="name">server</span><span class="pill on">lyra runs here</span></div></div>`,
    ...list.map((m) => `<div class="card">
      <div class="top"><span class="dot ${m.online ? "on" : ""}"></span><span class="name">${esc(m.name)}</span>
        <span class="pill ${m.online ? "on" : ""}">${m.online ? "online" : "offline"}</span>
        ${m.update_available ? '<span class="pill warn">update</span>' : ""}</div>
      <div class="meta">${m.online ? [m.hostname, m.os, m.user && "as " + m.user, m.version && (m.self_update ? "lyra-node " + m.version + " (" + m.build + ")" : "node built into lyra " + m.version)].filter(Boolean).map(esc).join(" · ") : "last seen " + esc(ago(m.last_seen))}</div>
      <div class="actions">
        ${m.online ? `<button data-mention="${esc(m.name)}">Ask on @${esc(m.name)}</button>` : ""}
        ${m.online && m.self_update ? `<button data-cmd="/machines update ${esc(m.name)}">${m.update_available ? "Update" : "Reinstall latest"}</button>` : ""}
        <button class="danger" data-cmd="/machines remove ${esc(m.name)}" data-confirm="Remove lyra-node from ${esc(m.name)}? ${m.online ? "It uninstalls itself and is unpaired." : "It's offline: it will only be unpaired; its files stay there."}">Remove</button>
      </div></div>`),
  ].join("");
  el.querySelectorAll("button[data-cmd]").forEach((b) => b.addEventListener("click", () => command(b.dataset.cmd, b.dataset.confirm)));
  el.querySelectorAll("button[data-mention]").forEach((b) => b.addEventListener("click", () => {
    showPage("chat");
    $("input").value = "@" + b.dataset.mention + " ";
    $("input").focus();
  }));
  $("install-cmd").textContent = `curl -fsSL ${location.origin}/install.sh | sh -s -- --name NAME`;
}
$("copy-install").addEventListener("click", async () => {
  try { await navigator.clipboard.writeText($("install-cmd").textContent); $("copy-install").textContent = "Copied"; } catch (e) { $("copy-install").textContent = "Select it"; }
  setTimeout(() => ($("copy-install").textContent = "Copy"), 2000);
});

function showData(what, data) {
  if (what === "devices") {
    const el = $("device-list");
    el.innerHTML = (data || []).map((d) => `<div class="card${state.device && d.id === state.device.id ? " current" : ""}">
      <div class="top"><span class="dot ${d.online ? "on" : ""}"></span><span class="name">${esc(d.name)}</span>
        <span class="pill">${d.kind === "node" ? "machine" : "device"}</span><span class="pill ${d.online ? "on" : ""}">${d.online ? "online" : "offline"}</span></div>
      <div class="meta">paired ${esc(ago(d.created))} · last seen ${esc(ago(d.last_seen))}${d.push ? " · notifications on" : ""}${state.device && d.id === state.device.id ? " · this device" : ""}</div>
      ${state.device && d.id === state.device.id ? "" : `<div class="actions"><button class="danger" data-remove="${esc(d.id)}" data-name="${esc(d.name)}">Unpair</button></div>`}
    </div>`).join("");
    el.querySelectorAll("button[data-remove]").forEach((b) => b.addEventListener("click", () => {
      command("/devices remove " + b.dataset.remove, `Unpair ${b.dataset.name}? It will need to pair again.`);
      setTimeout(() => ask("devices"), 800);
    }));
  } else if (what === "activity") {
    $("activity-list").innerHTML = (data || []).map((a) => `<div class="${esc(a.level)}"><span class="t">${esc(a.time)}</span>${esc(a.text)}</div>`).join("");
  } else if (what === "sessions") {
    const el = $("session-list");
    el.innerHTML = (data || []).map((s) => `<div class="card${s.current ? " current" : ""}" data-id="${esc(s.id)}">
      <div class="top"><span class="name">${esc(s.title || "(untitled)")}</span>${s.current ? '<span class="pill on">open</span>' : ""}</div>
      <div class="meta">${s.turns} turns · ${esc(ago(s.updated))}</div></div>`).join("") || '<p class="muted">No saved conversations yet.</p>';
    el.querySelectorAll(".card[data-id]").forEach((c) => c.addEventListener("click", () => {
      if (c.classList.contains("current")) { showPage("chat"); return; }
      command("/resume " + c.dataset.id);
      showPage("chat");
    }));
  } else if (what === "about") {
    $("about").innerHTML = `lyra ${esc(data.lyra || "")} · app ${esc(LYRA_VERSION)}${data.app && data.app !== LYRA_VERSION ? " (server has " + esc(data.app) + ")" : ""} · model ${esc(data.model || "")}<br>lyra-node on offer: ${esc(data.node_build || "none")} · ${data.devices} paired devices`;
  } else if (data && typeof data.text === "string") {
    const v = $("text-view");
    v.textContent = data.text;
    v.classList.remove("hidden");
  }
}
$("activity-refresh").addEventListener("click", () => ask("activity"));
document.querySelectorAll("button[data-text]").forEach((b) => b.addEventListener("click", () => ask(b.dataset.text)));
setInterval(() => { if (state.page === "activity" && document.visibilityState === "visible") ask("activity"); }, 5000);
$("new-chat").addEventListener("click", () => { command("/new"); showPage("chat"); });

// ---- updating the installed app

async function updateApp() {
  $("update-now").disabled = true;
  try {
    const reg = await navigator.serviceWorker.getRegistration();
    if (reg) await reg.update();
  } catch (e) {}
  location.reload();
}
$("update-now").addEventListener("click", updateApp);
$("check-update").addEventListener("click", async () => {
  const b = $("check-update");
  b.textContent = "Checking…";
  try {
    const reg = await navigator.serviceWorker.getRegistration();
    if (reg) await reg.update();
  } catch (e) {}
  if (state.serverVersion && state.serverVersion !== LYRA_VERSION) {
    $("update-banner").classList.remove("hidden");
    b.textContent = "Update available";
  } else {
    b.textContent = "Up to date (" + LYRA_VERSION + ")";
  }
  setTimeout(() => (b.textContent = "Check for app update"), 4000);
});
// A new service worker took over: load the new app.
if ("serviceWorker" in navigator) {
  let reloading = false;
  navigator.serviceWorker.addEventListener("controllerchange", () => {
    if (!reloading && state.serverVersion && state.serverVersion !== LYRA_VERSION) {
      reloading = true;
      location.reload();
    }
  });
}

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
  showPage("chat");
  connect();
}

// ---- boot

if ("serviceWorker" in navigator && window.isSecureContext) navigator.serviceWorker.register("/sw.js").catch(() => {});
try { state.token = localStorage.getItem("lyra-token"); } catch (e) {}
if (state.token) { idbSet("token", state.token); start(); } else showPair();
