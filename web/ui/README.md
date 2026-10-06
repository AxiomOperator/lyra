# lyra web app

React + Vite + TypeScript + Tailwind v4, with [shadcn/ui](https://ui.shadcn.com)
components and [Vercel AI Elements](https://ai-sdk.dev/elements) for the chat
(Conversation, Message/Response, Reasoning, Tool, PromptInput, CodeBlock).
It speaks lyra's own WebSocket protocol (`src/lyra/store.tsx`; the server side
is `src/serve.rs` and `web/src/lib.rs`).

```sh
npm install
npm run dev     # http://localhost:5173, proxied to a `lyra serve` on 127.0.0.1:8484
npm run build   # → dist/, which is embedded in the lyra binary
```

`dist/` is committed so lyra builds without Node: after changing the app, run
`npm run build` and commit `dist/` with the change. The server stamps the
app's version into `index.html` and `sw.js`, so installed apps offer the update.

Syntax highlighting uses a small Shiki setup with a fixed set of languages
(`src/lyra/highlight.ts`) instead of the stock everything-bundle.
