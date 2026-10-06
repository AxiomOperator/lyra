// Push notifications: subscribe through the browser's push service with
// lyra's VAPID key and register the subscription with lyra.

export const isIos = () => /iphone|ipad|ipod/i.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
export const standalone = () => window.matchMedia("(display-mode: standalone)").matches || (navigator as { standalone?: boolean }).standalone === true;

/** Why notifications can't be turned on here, if they can't. */
export function blocker(): string | null {
  if (!("serviceWorker" in navigator) || !window.isSecureContext) return "Notifications need lyra's https:// address (your reverse proxy).";
  if (isIos() && !standalone()) return "On iPhone, notifications work once lyra is on the Home Screen: Share → Add to Home Screen, then open it from there.";
  if (!("PushManager" in window)) return "This browser can't receive notifications.";
  if (Notification.permission === "denied") return "Notifications are blocked for this site in the browser or phone settings.";
  return null;
}

function b64ToBytes(s: string) {
  const pad = "=".repeat((4 - (s.length % 4)) % 4);
  const raw = atob((s + pad).replace(/-/g, "+").replace(/_/g, "/"));
  return Uint8Array.from(raw, (c) => c.charCodeAt(0));
}

const auth = (token: string) => ({ "Content-Type": "application/json", Authorization: "Bearer " + token });

export async function enable(token: string) {
  if ((await Notification.requestPermission()) !== "granted") throw new Error("permission wasn't granted");
  const reg = await navigator.serviceWorker.ready;
  const { key } = await (await fetch("/api/vapid")).json();
  const old = await reg.pushManager.getSubscription();
  if (old) await old.unsubscribe();
  const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64ToBytes(key) });
  const r = await fetch("/api/push", { method: "POST", headers: auth(token), body: JSON.stringify({ subscription: sub.toJSON() }) });
  if (!r.ok) throw new Error((await r.json()).error || r.statusText);
}

export async function disable(token: string) {
  try {
    const reg = await navigator.serviceWorker.ready;
    const sub = await reg.pushManager.getSubscription();
    if (sub) await sub.unsubscribe();
  } catch {
    // keep going: tell lyra anyway
  }
  await fetch("/api/push", { method: "POST", headers: auth(token), body: JSON.stringify({ subscription: null }) });
}

export async function test(token: string): Promise<string> {
  const r = await fetch("/api/test-push", { method: "POST", headers: { Authorization: "Bearer " + token } });
  return r.ok ? "Sent — it should arrive in a few seconds." : "Failed: " + ((await r.json()).error || r.statusText);
}

/** "3 min ago" */
export function ago(iso?: string | null) {
  if (!iso) return "";
  const s = (Date.now() - new Date(iso).getTime()) / 1000;
  if (s < 90) return "just now";
  if (s < 3600) return `${Math.round(s / 60)} min ago`;
  if (s < 86400) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86400)} days ago`;
}
