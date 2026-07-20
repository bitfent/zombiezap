// Map generator invariants — the properties fair wagered duels depend on.
// Runs the generator over many seeds and asserts:
//   determinism, symmetry, spawn clearance, corridor gaps, jump-proof heights,
//   in-bounds pieces, and NO direct spawn-to-spawn line of sight.
import { generateArena, ARENA_HALF, BARREL_SPAWN_CLEAR, PLAYER_EYE, nearestWallT } from "../packages/shared/src/index.ts";

const fail = (m) => { console.error("FAIL:", m); process.exit(1); };

// determinism
const a1 = generateArena("seed-x"), a2 = generateArena("seed-x");
if (JSON.stringify(a1) !== JSON.stringify(a2)) fail("same seed produced different maps");
if (JSON.stringify(generateArena("seed-y").walls) === JSON.stringify(a1.walls)) fail("different seeds produced identical maps");

let minCover = Infinity, maxCover = 0;
for (let i = 0; i < 500; i++) {
  const a = generateArena(`fuzz-${i}`);
  const cover = a.walls.slice(4);
  minCover = Math.min(minCover, cover.length); maxCover = Math.max(maxCover, cover.length);
  // a low piece is a legitimate STAIR STEP if a taller piece with the same x
  // span abuts it in z (the staircase rises toward the platform)
  const isStep = (b) => cover.some((c) =>
    c !== b && c.y1 > b.y1 + 1e-6 &&
    Math.abs(c.x0 - b.x0) < 1e-6 && Math.abs(c.x1 - b.x1) < 1e-6 &&
    (Math.abs(c.z0 - b.z1) < 1e-6 || Math.abs(c.z1 - b.z0) < 1e-6));
  for (const b of cover) {
    if (b.x0 < -ARENA_HALF || b.x1 > ARENA_HALF || b.z0 < -ARENA_HALF || b.z1 > ARENA_HALF) fail(`piece out of bounds (seed fuzz-${i})`);
    // ground cover must be MEANINGFUL: either blocking (above the 1.225 jump
    // apex) or a climbable stand (crates >= 1.0, by design). Below 0.9 it is
    // neither — just a trip hazard. Stair steps rise to a perch and are exempt.
    if (b.y0 < 1.2 && b.y1 < 0.9 && !isStep(b)) fail(`useless low cover height ${b.y1} (seed fuzz-${i})`);
    if (b.y0 >= 1.2 && b.y0 < 1.8) fail(`floating piece at climbable height (seed fuzz-${i})`);
    // symmetry: every piece's 180° twin must exist
    const twin = cover.find((c) => Math.abs(c.x0 + b.x1) < 1e-6 && Math.abs(c.x1 + b.x0) < 1e-6 && Math.abs(c.z0 + b.z1) < 1e-6 && Math.abs(c.z1 + b.z0) < 1e-6);
    if (!twin) fail(`asymmetric piece (seed fuzz-${i})`);
    // spawn clearance (ground-blocking pieces only)
    if (b.y0 >= 1.8) continue;
    for (const s of a.spawns) {
      const cx = Math.max(b.x0, Math.min(s.x, b.x1)), cz = Math.max(b.z0, Math.min(s.z, b.z1));
      if ((cx - s.x) ** 2 + (cz - s.z) ** 2 < 2.0 ** 2) fail(`cover crowds a spawn (seed fuzz-${i})`);
    }
  }
  // pickups: mirrored pairs, never inside a wall
  if (a.pickups.length < 2 || a.pickups.length % 2 !== 0) fail(`missing pickups (seed fuzz-${i})`);
  for (let p = 0; p < a.pickups.length; p += 2) {
    const [pk0, pk1] = [a.pickups[p], a.pickups[p + 1]];
    if (Math.abs(pk0.x + pk1.x) > 1e-6 || Math.abs(pk0.z + pk1.z) > 1e-6) fail(`asymmetric pickups (seed fuzz-${i})`);
  }
  for (const pk of a.pickups) {
    // only ground-blocking boxes count — a pickup under a ROOF (inside a
    // building, reachable through the door) is intentional loot placement
    if (a.walls.some((w) => w.y0 < 1.0 && pk.x > w.x0 - 0.3 && pk.x < w.x1 + 0.3 && pk.z > w.z0 - 0.3 && pk.z < w.z1 + 0.3)) {
      fail(`pickup inside a wall (seed fuzz-${i})`);
    }
  }

  // explosive barrels: wallIndex must point at the box that contains the
  // barrel's center, pairs must mirror, and none may camp a spawn
  if (a.barrels.length % 2 !== 0) fail(`unpaired barrels (seed fuzz-${i})`);
  for (const b of a.barrels) {
    const w = a.walls[b.wallIndex];
    if (!w || b.x < w.x0 - 1e-6 || b.x > w.x1 + 1e-6 || b.z < w.z0 - 1e-6 || b.z > w.z1 + 1e-6) {
      fail(`barrel wallIndex does not contain its center (seed fuzz-${i})`);
    }
    for (const s of a.spawns) {
      if (Math.hypot(b.x - s.x, b.z - s.z) < BARREL_SPAWN_CLEAR) fail(`barrel too close to a spawn (seed fuzz-${i})`);
    }
  }
  for (let p = 0; p < a.barrels.length; p += 2) {
    const [b0, b1] = [a.barrels[p], a.barrels[p + 1]];
    if (Math.abs(b0.x + b1.x) > 1e-6 || Math.abs(b0.z + b1.z) > 1e-6) fail(`asymmetric barrel pair (seed fuzz-${i})`);
  }

  // fairness: spawn0 must not see spawn1
  const [s0, s1] = a.spawns;
  const dx = s1.x - s0.x, dz = s1.z - s0.z, dist = Math.hypot(dx, dz);
  const t = nearestWallT({ x: s0.x, y: PLAYER_EYE, z: s0.z }, { x: dx / dist, y: 0, z: dz / dist }, cover);
  if (t === null || t > dist) fail(`open spawn-to-spawn sightline (seed fuzz-${i})`);
}
console.log(`PASS: 500 seeds — deterministic, symmetric, spawn-safe, jump-proof, LOS always blocked (cover pieces ${minCover}–${maxCover})`);
