// Load .env into process.env for LOCAL dev, before any module reads config.
// Must be the FIRST import in index.ts so escrow/price/etc. see the values at
// their own import time. On hosts like Render the env is injected directly (no
// .env file present), so this is a no-op there. Bun auto-loads .env and may lack
// process.loadEnvFile, so it's guarded.
import { existsSync } from "node:fs";

if (existsSync(".env") && typeof process.loadEnvFile === "function") {
  try {
    process.loadEnvFile(".env");
  } catch {
    /* malformed .env — fall through to whatever's already in the environment */
  }
}
