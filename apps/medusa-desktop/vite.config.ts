import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { configDefaults } from "vitest/config";

export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  server: {
    strictPort: true,
    port: 5173,
  },
  envPrefix: ["VITE_", "TAURI_"],
  test: {
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts",
    exclude: [...configDefaults.exclude, "src-tauri/**"],
    // Packaging jobs run the full jsdom suite beside native bundler prerequisites on bounded
    // runners. Keep a finite budget, but allow integration-style App renders to survive CPU
    // contention without turning a successful accessibility assertion into a scheduler timeout.
    testTimeout: 10_000,
  },
});
