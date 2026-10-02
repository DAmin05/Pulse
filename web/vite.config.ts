import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

// The Query API; override with PULSE_API (e.g. a dev instance on another port).
const api = process.env.PULSE_API ?? "http://localhost:9105";

export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
    proxy: {
      "/api": { target: api, changeOrigin: true },
    },
  },
  preview: {
    port: 4173,
    proxy: { "/api": { target: api, changeOrigin: true } },
  },
  test: {
    environment: "node",
  },
});
