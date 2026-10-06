// lyra's service worker: shows push notifications (with Allow / Deny on
// approvals where the platform supports buttons), answers them, and keeps
// the app shell available offline.
"use strict";

// A new version of the app gets a new cache (the old one is dropped on activate).
const CACHE = "lyra-shell-__LYRA_VERSION__";
const SHELL = ["/", "/manifest.webmanifest", "/icon-192.png", "/apple-touch-icon.png"];

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys().then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k)))).then(() => self.clients.claim()),
  );
});

// The app: network first (always the latest), the cache when offline. The
// bundle's files have content hashes in their names, so caching them is safe.
// "Share to lyra" (Android): keep what was shared, then open lyra, which
// puts it in the composer (the token stays with the page).
function keepShared(value) {
  return new Promise((resolve) => {
    const r = indexedDB.open("lyra", 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onerror = () => resolve();
    r.onsuccess = () => {
      const tx = r.result.transaction("kv", "readwrite");
      tx.objectStore("kv").put(value, "share");
      tx.oncomplete = () => resolve();
      tx.onerror = () => resolve();
    };
  });
}

self.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (e.request.method === "POST" && url.pathname === "/share") {
    e.respondWith(
      (async () => {
        try {
          const form = await e.request.formData();
          const text = ["title", "text", "url"].map((k) => form.get(k)).filter((v) => typeof v === "string" && v.trim()).join("\n");
          const files = form.getAll("files").filter((f) => typeof f === "object" && f.size > 0);
          await keepShared({ text, files, at: Date.now() });
        } catch (err) {
          // nothing kept: lyra just opens
        }
        return Response.redirect("/?shared=1", 303);
      })(),
    );
    return;
  }
  if (e.request.method !== "GET" || url.origin !== location.origin || url.pathname.startsWith("/api/") || url.pathname === "/ws") return;
  const keep = SHELL.includes(url.pathname) || url.pathname.startsWith("/assets/");
  e.respondWith(
    fetch(e.request)
      .then((r) => {
        if (r.ok && keep) {
          const copy = r.clone();
          caches.open(CACHE).then((c) => c.put(url.pathname, copy));
        }
        return r;
      })
      .catch(() => caches.match(url.pathname)),
  );
});

self.addEventListener("push", (e) => {
  let data = {};
  try { data = e.data ? e.data.json() : {}; } catch (err) { data = { body: e.data && e.data.text() }; }
  const approval = data.approval || null;
  const options = {
    body: data.body || "",
    tag: data.tag || "lyra",
    renotify: true,
    icon: "/icon-192.png",
    badge: "/icon-192.png",
    data: { approval, url: approval ? "/?approval=" + approval : "/" },
  };
  if (approval) {
    options.requireInteraction = true;
    options.actions = [
      { action: "allow", title: "Allow" },
      { action: "deny", title: "Deny" },
    ];
  }
  e.waitUntil(self.registration.showNotification(data.title || "lyra", options));
});

function token() {
  return new Promise((resolve) => {
    const r = indexedDB.open("lyra", 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onerror = () => resolve(null);
    r.onsuccess = () => {
      const get = r.result.transaction("kv").objectStore("kv").get("token");
      get.onsuccess = () => resolve(get.result || null);
      get.onerror = () => resolve(null);
    };
  });
}

async function openApp(url) {
  const all = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  for (const c of all) {
    if ("focus" in c) { await c.focus(); return; }
  }
  await self.clients.openWindow(url);
}

self.addEventListener("notificationclick", (e) => {
  e.notification.close();
  const { approval, url } = e.notification.data || {};
  if (approval && (e.action === "allow" || e.action === "deny")) {
    e.waitUntil(
      token().then((t) =>
        t
          ? fetch("/api/approve", {
              method: "POST",
              headers: { "Content-Type": "application/json", Authorization: "Bearer " + t },
              body: JSON.stringify({ id: approval, answer: e.action === "allow" ? "y" : "n" }),
            }).then((r) => (r.ok ? null : openApp(url)))
          : openApp(url),
      ),
    );
    return;
  }
  e.waitUntil(openApp(url || "/"));
});
