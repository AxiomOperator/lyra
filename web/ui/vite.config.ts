import path from "node:path";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Built into lyra (web/ui/dist is embedded in the binary); `npm run dev`
// proxies the API and WebSocket to a local `lyra serve`.
export default defineConfig({
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": path.resolve(__dirname, "./src") } },
  // The warning limit keeps the first load in check (D-5: pages and Shiki load on demand).
  build: { outDir: "dist", emptyOutDir: true, chunkSizeWarningLimit: 1100 },
  server: {
    proxy: {
      "/api": "http://127.0.0.1:8484",
      "/ws": { target: "ws://127.0.0.1:8484", ws: true },
      "/download": "http://127.0.0.1:8484",
      "/install.sh": "http://127.0.0.1:8484",
      "/sw.js": "http://127.0.0.1:8484",
    },
  },
});
