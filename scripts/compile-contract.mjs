// Compile MatchEscrow.sol with Foundry (forge build) and extract the slim
// { abi, bytecode } artifact at contracts/out/MatchEscrow.json that the
// deploy + rehearsal scripts read. Run: npm run compile:contract
//
// Replaces the old solc-js path — forge uses solc 0.8.24 / optimizer 200 runs
// (contracts/foundry.toml), the same settings, so the bytecode is equivalent.

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const CONTRACTS = join(ROOT, "contracts");
const OUT_DIR = join(CONTRACTS, "out");

execFileSync("forge", ["build", "--root", CONTRACTS], { stdio: "inherit" });

const art = JSON.parse(
  readFileSync(join(CONTRACTS, "forge-out/MatchEscrow.sol/MatchEscrow.json"), "utf8"),
);

mkdirSync(OUT_DIR, { recursive: true });
writeFileSync(
  join(OUT_DIR, "MatchEscrow.json"),
  JSON.stringify({ abi: art.abi, bytecode: art.bytecode.object }, null, 2),
);
const bytes = (art.bytecode.object.length - 2) / 2;
console.log(`compiled OK -> contracts/out/MatchEscrow.json (bytecode ${bytes} bytes)`);
