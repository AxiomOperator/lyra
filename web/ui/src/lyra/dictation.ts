// Dictation: the whole transcript so far, from the browser's speech results.

/** One recognized phrase (a `SpeechRecognitionResult`'s best guess). */
export type Phrase = { transcript: string };

/**
 * Everything said since dictation started, interim words included. Each call
 * rebuilds it from all results, so the composer replaces what was dictated
 * instead of appending. Chrome on Android sends a phrase again each time it
 * grows ("tell", "tell me", "tell me the"…), each marked final; only the
 * longest version of a growing phrase is kept.
 */
export function transcript(phrases: Phrase[]): string {
  const parts: string[] = [];
  for (const p of phrases) {
    const t = p.transcript.trim().replace(/\s+/g, " ");
    if (!t) continue;
    const last = parts[parts.length - 1]?.toLowerCase();
    const now = t.toLowerCase();
    if (last !== undefined && now.startsWith(last)) parts[parts.length - 1] = t;
    else if (last !== undefined && last.startsWith(now)) continue;
    else parts.push(t);
  }
  return parts.join(" ");
}

/** What was typed before dictation started, then what's being said. */
export function withDictation(before: string, said: string): string {
  if (!said) return before;
  return before.trim() ? `${before.trimEnd()} ${said}` : said;
}
