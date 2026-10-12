// Tool calls' arguments and results, parsed once each: a streaming reply
// re-renders the chat many times a second, and the same text would otherwise
// be parsed again every time. The parsed values are shared: don't change them.

const FAILED = Symbol("not JSON");
const cache = new Map<string, unknown>();
const MAX = 400;

/** `text` as JSON, or `undefined` when it isn't JSON. */
export function parseJson(text: string): unknown {
  let v = cache.get(text);
  if (v === undefined) {
    try {
      v = JSON.parse(text);
    } catch {
      v = FAILED;
    }
    // The oldest goes first (a Map keeps the order things were put in).
    if (cache.size >= MAX) cache.delete(cache.keys().next().value as string);
    cache.set(text, v);
  }
  return v === FAILED ? undefined : v;
}
