import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Tauri serves the dev build from a fixed port and expects a static bundle in
// ../dist for release builds.
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    target: "es2022",
    sourcemap: true,
  },
});
