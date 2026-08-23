import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const host = process.env["TAURI_DEV_HOST"];

export default defineConfig({
  plugins: [
    react({ babel: { plugins: [["babel-plugin-react-compiler", {}]] } }),
    tailwindcss(),
  ],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host ?? false,
    ...(host === undefined
      ? {}
      : { hmr: { protocol: "ws", host, port: 1421 } }),
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
});
