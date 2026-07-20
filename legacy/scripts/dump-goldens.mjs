// Dump golden-test fixtures from the legacy TypeScript sim for the Rust zz-core port.
//
// Usage (from legacy/ or repo root):
//   node --experimental-strip-types legacy/scripts/dump-goldens.mjs
//   node --experimental-strip-types scripts/dump-goldens.mjs
//
// Writes three JSON files into crates/zz-core/tests/fixtures/.

import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// Import individual modules (not index.ts) — index re-exports escrow.ts which
// is not always present in this worktree, and we only need sim primitives.
import { seededRandom, generateArena } from "../packages/shared/src/mapgen.ts";
import { stepBody } from "../packages/shared/src/movement.ts";
import { TICK_DT, PLAYER_SPEED } from "../packages/shared/src/constants.ts";
import { ARENA_HALF } from "../packages/shared/src/arena.ts";

const __dirname = dirname(fileURLToPath(import.meta.url));
// legacy/scripts -> repo root is ../..
const REPO_ROOT = join(__dirname, "..", "..");
const OUT_DIR = join(REPO_ROOT, "crates", "zz-core", "tests", "fixtures");

mkdirSync(OUT_DIR, { recursive: true });

function writeJson(name, value) {
  const path = join(OUT_DIR, name);
  // plain JSON.stringify for full double precision (no pretty-print)
  writeFileSync(path, JSON.stringify(value) + "\n", "utf8");
  console.log(`wrote ${path}`);
}

// --- 1. rng_golden.json ------------------------------------------------------
{
  const seed = "zz-golden-1";
  const rand = seededRandom(seed);
  const floats = [];
  for (let i = 0; i < 1000; i++) floats.push(rand());
  writeJson("rng_golden.json", { seed, floats });
}

// --- 2. mapgen_golden.json ---------------------------------------------------
{
  const arenas = {
    "zz-a": generateArena("zz-a"),
    "zz-b": generateArena("zz-b"),
    "zz-c": generateArena("zz-c"),
  };
  writeJson("mapgen_golden.json", { arenas });
}

// --- 3. movement_golden.json -------------------------------------------------
{
  const mapSeed = "zz-golden-map";
  const arena = generateArena(mapSeed);
  const spawn = arena.spawns[0];
  const body = {
    x: spawn.x,
    y: 0,
    z: spawn.z,
    vy: 0,
    onGround: true,
  };

  const steps = [];
  for (let i = 0; i < 300; i++) {
    const input = {
      sequence: i,
      forward: (i % 97) < 60,
      backward: (i % 89) > 70,
      left: (i % 53) < 20,
      right: (i % 71) > 50,
      jump: (i % 45) === 0,
      shoot: false,
      yaw: i * 0.037,
      pitch: 0,
    };
    stepBody(body, input, TICK_DT, arena.walls);
    steps.push([body.x, body.y, body.z, body.vy, body.onGround ? 1 : 0]);
  }

  writeJson("movement_golden.json", {
    mapSeed,
    arenaHalf: ARENA_HALF,
    playerSpeed: PLAYER_SPEED,
    dt: TICK_DT,
    start: {
      x: spawn.x,
      y: 0,
      z: spawn.z,
      vy: 0,
      onGround: true,
    },
    steps,
  });
}

console.log("done.");
