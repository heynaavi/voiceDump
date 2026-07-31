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
    // Overridable so a second dev server can run alongside the first
    // instead of fighting it for the port.
    // @ts-expect-error process is a nodejs global
    port: Number(process.env.VD_PORT) || 1420,
    strictPort: true,
    host: host || false,
    // @ts-expect-error process is a nodejs global
    hmr: host ? { protocol: "ws", host, port: (Number(process.env.VD_PORT) || 1420) + 1 } : undefined,
    watch: {
      // Rust sources and build output are not the dev server's business, and
      // `src-tauri/target` alone is large enough to make the watcher struggle.
      ignored: ["**/src-tauri/**", "**/models/**"],
    },
  },
}));
