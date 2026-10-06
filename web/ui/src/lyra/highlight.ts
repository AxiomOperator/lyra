// Syntax highlighting with only the languages lyra's chats use (the stock
// Streamdown/AI Elements setup ships every Shiki grammar and the WebAssembly
// regex engine: hundreds of files, megabytes). Grammars load on first use.

import type { CodeHighlighterPlugin } from "@streamdown/code";
import type { HighlighterCore, LanguageInput, TokensResult } from "shiki/core";
import { createHighlighterCore } from "shiki/core";
import { createJavaScriptRegexEngine } from "shiki/engine/javascript";

const grammars: Record<string, () => Promise<{ default: LanguageInput }>> = {
  bash: () => import("shiki/langs/bash.mjs"),
  shellsession: () => import("shiki/langs/shellsession.mjs"),
  json: () => import("shiki/langs/json.mjs"),
  jsonc: () => import("shiki/langs/jsonc.mjs"),
  yaml: () => import("shiki/langs/yaml.mjs"),
  toml: () => import("shiki/langs/toml.mjs"),
  ini: () => import("shiki/langs/ini.mjs"),
  rust: () => import("shiki/langs/rust.mjs"),
  python: () => import("shiki/langs/python.mjs"),
  javascript: () => import("shiki/langs/javascript.mjs"),
  typescript: () => import("shiki/langs/typescript.mjs"),
  tsx: () => import("shiki/langs/tsx.mjs"),
  jsx: () => import("shiki/langs/jsx.mjs"),
  html: () => import("shiki/langs/html.mjs"),
  css: () => import("shiki/langs/css.mjs"),
  sql: () => import("shiki/langs/sql.mjs"),
  diff: () => import("shiki/langs/diff.mjs"),
  markdown: () => import("shiki/langs/markdown.mjs"),
  go: () => import("shiki/langs/go.mjs"),
  c: () => import("shiki/langs/c.mjs"),
  java: () => import("shiki/langs/java.mjs"),
  docker: () => import("shiki/langs/docker.mjs"),
  nginx: () => import("shiki/langs/nginx.mjs"),
  xml: () => import("shiki/langs/xml.mjs"),
  powershell: () => import("shiki/langs/powershell.mjs"),
  lua: () => import("shiki/langs/lua.mjs"),
};

const aliases: Record<string, string> = {
  sh: "bash", shell: "bash", zsh: "bash", console: "shellsession", terminal: "shellsession",
  js: "javascript", mjs: "javascript", cjs: "javascript", ts: "typescript", py: "python", rs: "rust",
  yml: "yaml", md: "markdown", dockerfile: "docker", conf: "ini", cfg: "ini", ps1: "powershell", golang: "go", htm: "html",
};

/** A language lyra highlights, or "text". */
export function resolveLanguage(lang: string): string {
  const l = (lang || "").trim().toLowerCase();
  const id = aliases[l] ?? l;
  return id in grammars ? id : "text";
}

let core: Promise<HighlighterCore> | null = null;

/** The highlighter, with `lang` loaded (or "text"). */
export async function getHighlighter(lang: string): Promise<{ highlighter: HighlighterCore; lang: string }> {
  core ??= createHighlighterCore({
    themes: [import("shiki/themes/github-light.mjs"), import("shiki/themes/github-dark.mjs")],
    langs: [],
    engine: createJavaScriptRegexEngine({ forgiving: true }),
  });
  const highlighter = await core;
  const id = resolveLanguage(lang);
  if (id !== "text" && !highlighter.getLoadedLanguages().includes(id)) {
    await highlighter.loadLanguage((await grammars[id]()).default);
  }
  return { highlighter, lang: id };
}

const cache = new Map<string, TokensResult>();

/** Streamdown's code plugin, on the small highlighter above. */
export const lyraCode: CodeHighlighterPlugin = {
  name: "shiki",
  type: "code-highlighter",
  supportsLanguage: (lang) => resolveLanguage(lang) !== "text",
  getSupportedLanguages: () => Object.keys(grammars) as never,
  getThemes: () => ["github-light", "github-dark"],
  highlight({ code, language, isIncomplete }, callback) {
    const key = `${resolveLanguage(language)}\n${code}`;
    const hit = cache.get(key);
    if (hit) return hit;
    getHighlighter(language)
      .then(({ highlighter, lang }) => {
        const result = highlighter.codeToTokens(code, { lang, themes: { light: "github-light", dark: "github-dark" } });
        if (!isIncomplete) {
          if (cache.size > 500) cache.clear();
          cache.set(key, result);
        }
        callback?.(result);
      })
      .catch((e) => console.error("[lyra] highlighting failed:", e));
    return null;
  },
};
