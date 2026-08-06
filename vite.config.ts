import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],

  // Don't let Vite's screen-clear hide Rust compile errors.
  clearScreen: false,
  server: {
    // Overridable so the lite build can run next to the full qwee build
    // instead of fighting it for the port — testing the standalone app
    // shouldn't mean taking the team's Slack assistant offline.
    // @ts-expect-error process is a nodejs global
    port: Number(process.env.VD_PORT) || 1420,
    strictPort: true,
    host: host || false,
    // @ts-expect-error process is a nodejs global
    hmr: host ? { protocol: "ws", host, port: (Number(process.env.VD_PORT) || 1420) + 1 } : undefined,
    watch: {
      // `.claude/worktrees` holds full checkouts of this same project, so
      // without this the dev server watches a second copy of its own source.
      ignored: ["**/src-tauri/**", "**/sidecar/**", "**/.claude/**"],
    },
  },
}));
