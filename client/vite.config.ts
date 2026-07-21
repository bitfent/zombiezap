import { defineConfig } from "vite";
import { fileURLToPath } from "node:url";

export default defineConfig({
  resolve: {
    alias: {
      // Copied-forward ShotAnte modules keep their original import specifier
      // so game/network files run unedited where possible.
      "@shotante/shared": fileURLToPath(new URL("./src/shared/index.ts", import.meta.url)),
    },
  },
  server: {
    port: 5174,
  },
});
