// Renderer build for the Tauri shell (src-tauri). Mirrors the `renderer` target
// in electron.vite.config.ts, plus aliases that let the Electron preload run in-page.
import react from "@vitejs/plugin-react";
import { resolve } from "path";
import { defineConfig } from "vite";
import svgr from "vite-plugin-svgr";
import { scopeBigPictureCss } from "./src/big-picture/vite-scope-big-picture-css";

export default defineConfig(({ mode }) => ({
  root: "src/renderer",
  envDir: resolve("."),
  envPrefix: ["RENDERER_VITE_", "VITE_"],
  server: { port: 5174, strictPort: true },
  build: {
    outDir: resolve("out/tauri"),
    emptyOutDir: true,
    // The renderer uses top-level await; WebView2 and WebKitGTK 2.38+ support it.
    target: ["es2022", "chrome105", "safari15"],
    sourcemap: mode !== "production",
    rollupOptions: { input: resolve("src/renderer/tauri.html") },
  },
  esbuild: { keepNames: true },
  css: {
    postcss: { plugins: [scopeBigPictureCss()] },
    preprocessorOptions: { scss: { api: "modern" } },
  },
  resolve: {
    alias: {
      electron: resolve("src/tauri/electron-shim.ts"),
      "node:crypto": resolve("src/tauri/node-crypto-shim.ts"),
      "@renderer": resolve("src/renderer/src"),
      "@locales": resolve("src/locales"),
      "@shared": resolve("src/shared"),
    },
  },
  plugins: [svgr(), react()],
}));
