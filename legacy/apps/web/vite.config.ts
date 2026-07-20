import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

export default defineConfig({
  // Base Account opens a passkey popup to account.base.org. The page must not
  // send Cross-Origin-Opener-Policy: same-origin or the popup can't talk back.
  // PRODUCTION: set the same header (same-origin-allow-popups) on the host
  // serving the web app, or the Base sign-in popup will hang.
  server: {
    headers: { "Cross-Origin-Opener-Policy": "same-origin-allow-popups" },
  },
  preview: {
    headers: { "Cross-Origin-Opener-Policy": "same-origin-allow-popups" },
  },
  resolve: {
    alias: {
      // Workspace package resolved straight to TS source — Vite transpiles it.
      "@shotante/shared": fileURLToPath(
        new URL("../../packages/shared/src/index.ts", import.meta.url),
      ),
    },
  },
});
