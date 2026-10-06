// The device token: localStorage for the page, IndexedDB for the service
// worker (it answers approvals from notification buttons).

function idb(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const r = indexedDB.open("lyra", 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onsuccess = () => resolve(r.result);
    r.onerror = () => reject(r.error);
  });
}

async function idbSet(key: string, value: string | null) {
  try {
    const db = await idb();
    const tx = db.transaction("kv", "readwrite");
    if (value === null) tx.objectStore("kv").delete(key);
    else tx.objectStore("kv").put(value, key);
  } catch {
    // Private mode: notification buttons just open the app instead.
  }
}

export function loadToken(): string | null {
  try {
    const t = localStorage.getItem("lyra-token");
    if (t) void idbSet("token", t);
    return t;
  } catch {
    return null;
  }
}

export function saveToken(t: string | null) {
  try {
    if (t) localStorage.setItem("lyra-token", t);
    else localStorage.removeItem("lyra-token");
  } catch {
    // ignore
  }
  void idbSet("token", t);
}
