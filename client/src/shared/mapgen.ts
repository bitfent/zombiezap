// Seeded procedural URBAN arenas (v5, Krunker-flavored, 60x60): a city
// generated fresh per match from a seed the server picks and ships in
// match_start. Client and server run THIS generator, so rendering, predicted
// collision, and authoritative hitscan all agree. Practice seeds locally.
//
// Elements: enterable buildings (doors + shoot-through window slits + flat
// roofs; big ones get TWO-ROOM interiors, tall ones a roof PARAPET), STAIRS up
// to rooftop PLATFORMS, CATWALK BRIDGES (walk over, duck under), SNIPER
// TOWERS (3m perches), L-shaped corner walls, crates/barriers/pillars/barrels
// for street cover, and mirrored health pickups. With step-up movement
// (movement.ts) stairs and crates are climbable, so the map plays in 3D —
// fairly, because everything is mirrored.
//
// Window trick: a sill (0->1.3) + a header (1.9->roof) leave a 1.3-1.9 slit —
// the player capsule overlaps both (can't pass) but eye height 1.55 shoots
// through. All falls out of the shared box physics.
//
// Fairness (wager game): 180° rotational symmetry (equal spawns), spawn
// corners clear, no direct spawn-to-spawn sight, >=1.5m streets so nothing
// seals, and a tall perimeter kept clear of climbables so the arena can't be
// escaped. Pickups come in mirrored pairs.

import { ARENA_HALF, type Box, type Spawn } from "./arena.ts";
import { BARREL_SPAWN_CLEAR, PLAYER_EYE } from "./constants.ts";
import { nearestWallT } from "./raycast.ts";

export interface Arena {
  seed: string;
  walls: Box[];
  spawns: Spawn[];
  pickups: { x: number; z: number }[];
  accent: number;
  // Explosive props: each entry points at its collision box in `walls` so
  // server and client can remove the SAME box when one detonates.
  barrels: { x: number; z: number; wallIndex: number }[];
}

function hashSeed(s: string): number {
  let h = 2166136261 >>> 0;
  for (let i = 0; i < s.length; i++) {
    h ^= s.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

function mulberry32(a: number): () => number {
  return () => {
    a |= 0;
    a = (a + 0x6d2b79f5) | 0;
    let t = Math.imul(a ^ (a >>> 15), 1 | a);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const ACCENTS = [0x2ee6d6, 0xff3355, 0xffd23f, 0xb478ff, 0x6cff8a, 0xff8a3d];

const WALL_H = 6; // perimeter — tall enough to be unclimbable from any perch
const T = 0.6;
const EDGE_MARGIN = 1.5;
const GAP = 1.6;
const SPAWN_CLEAR = 3.5;
const DOOR = 1.8;
const BWALL_T = 0.4;
const BUILD_H = 3.2;
const BUILD_H_TALL = 4.0; // occasional taller building with a roof parapet
const SILL_H = 1.3;
const HEAD_Y = 1.9;
const ROOF_T = 0.35;
const PARAPET_H = 0.45;
const STEP_RISE = 0.5;
const STEP_RUN = 0.8;
const PLATFORM_H = 2.0;
const TOWER_H = 3.0; // sniper perch — still 3m under the perimeter top
// Catwalk deck: top flush with the platforms (2.0), bottom at 1.85 so the
// 1.8m player capsule walks UNDER it — over/under play from plain boxes.
const DECK_TOP = PLATFORM_H;
const DECK_BOTTOM = 1.85;

function perimeter(): Box[] {
  const H = ARENA_HALF;
  return [
    { x0: -H - T, x1: H + T, y0: 0, y1: WALL_H, z0: -H - T, z1: -H },
    { x0: -H - T, x1: H + T, y0: 0, y1: WALL_H, z0: H, z1: H + T },
    { x0: -H - T, x1: -H, y0: 0, y1: WALL_H, z0: -H, z1: H },
    { x0: H, x1: H + T, y0: 0, y1: WALL_H, z0: -H, z1: H },
  ];
}

// Spawns track the arena size — always 4m in from the corners.
const SPAWN_D = ARENA_HALF - 4;
const SPAWNS: Spawn[] = [
  { x: -SPAWN_D, z: -SPAWN_D, yaw: -Math.PI * 0.75 },
  { x: SPAWN_D, z: SPAWN_D, yaw: Math.PI * 0.25 },
  { x: -SPAWN_D, z: SPAWN_D, yaw: -Math.PI * 0.25 },
  { x: SPAWN_D, z: -SPAWN_D, yaw: Math.PI * 0.75 },
];

function inflate(b: Box, m: number): Box {
  return { x0: b.x0 - m, x1: b.x1 + m, y0: b.y0, y1: b.y1, z0: b.z0 - m, z1: b.z1 + m };
}
function overlapsXZ(a: Box, b: Box): boolean {
  return a.x0 < b.x1 && a.x1 > b.x0 && a.z0 < b.z1 && a.z1 > b.z0;
}
function nearSpawn(b: Box, clear = SPAWN_CLEAR): boolean {
  for (const s of SPAWNS) {
    const cx = Math.max(b.x0, Math.min(s.x, b.x1));
    const cz = Math.max(b.z0, Math.min(s.z, b.z1));
    if ((cx - s.x) ** 2 + (cz - s.z) ** 2 < clear * clear) return true;
  }
  return false;
}
function mirror(b: Box): Box {
  return { x0: -b.x1, x1: -b.x0, y0: b.y0, y1: b.y1, z0: -b.z1, z1: -b.z0 };
}
const snap = (v: number) => Math.round(v * 2) / 2;

// ── building walls with door/window features ───────────────────────────────
function wallRun(
  axis: "x" | "z",
  fixed: number,
  from: number,
  to: number,
  feature: "solid" | "door" | "window",
  rng: () => number,
  h: number = BUILD_H,
): Box[] {
  const half = BWALL_T / 2;
  const seg = (a: number, b: number, y0: number, y1: number): Box =>
    axis === "x"
      ? { x0: a, x1: b, y0, y1, z0: fixed - half, z1: fixed + half }
      : { x0: fixed - half, x1: fixed + half, y0, y1, z0: a, z1: b };
  if (feature === "solid" || to - from < DOOR + 1.4) return [seg(from, to, 0, h)];
  const span = feature === "door" ? DOOR : Math.min(2.8, Math.max(2.0, (to - from) * 0.45));
  const at = snap(from + span / 2 + 0.6 + rng() * (to - from - span - 1.2));
  const a0 = at - span / 2;
  const a1 = at + span / 2;
  const out: Box[] = [];
  if (a0 - from > 0.3) out.push(seg(from, a0, 0, h));
  if (to - a1 > 0.3) out.push(seg(a1, to, 0, h));
  if (feature === "window") {
    out.push(seg(a0, a1, 0, SILL_H));
    out.push(seg(a0, a1, HEAD_Y, h));
  }
  return out;
}

function makeBuilding(rng: () => number, cx: number, cz: number, w: number, d: number): Box[] {
  const x0 = snap(cx - w / 2), x1 = snap(cx + w / 2);
  const z0 = snap(cz - d / 2), z1 = snap(cz + d / 2);
  const tall = rng() < 0.3; // taller variant reads as a landmark
  const h = tall ? BUILD_H_TALL : BUILD_H;
  const sides: ("door" | "window" | "solid")[] = [
    "door",
    "window",
    rng() < 0.5 ? "door" : "window",
    rng() < 0.6 ? "window" : "solid",
  ];
  for (let i = sides.length - 1; i > 0; i--) {
    const j = Math.floor(rng() * (i + 1));
    [sides[i], sides[j]] = [sides[j], sides[i]];
  }
  const out = [
    ...wallRun("x", z0, x0, x1, sides[0], rng, h),
    ...wallRun("x", z1, x0, x1, sides[1], rng, h),
    ...wallRun("z", x0, z0, z1, sides[2], rng, h),
    ...wallRun("z", x1, z0, z1, sides[3], rng, h),
    { x0, x1, y0: h, y1: h + ROOF_T, z0, z1 }, // flat roof
  ];
  // two-room interior: bigger footprints get a partition wall with a doorway —
  // room-to-room fights instead of one open box
  if (Math.max(w, d) >= 7) {
    if (w >= d) {
      const mid = snap(cx + (rng() - 0.5) * (w * 0.25));
      out.push(...wallRun("z", mid, z0 + BWALL_T, z1 - BWALL_T, "door", rng, h));
    } else {
      const mid = snap(cz + (rng() - 0.5) * (d * 0.25));
      out.push(...wallRun("x", mid, x0 + BWALL_T, x1 - BWALL_T, "door", rng, h));
    }
  }
  // parapet lip on tall roofs — partial head cover for anyone who gets up there
  if (tall) {
    const py0 = h + ROOF_T;
    const py1 = py0 + PARAPET_H;
    const lip = 0.25;
    out.push(
      { x0, x1, y0: py0, y1: py1, z0, z1: z0 + lip },
      { x0, x1, y0: py0, y1: py1, z0: z1 - lip, z1 },
      { x0, x1: x0 + lip, y0: py0, y1: py1, z0, z1 },
      { x0: x1 - lip, x1, y0: py0, y1: py1, z0, z1 },
    );
  }
  return out;
}

// ── platform + stairs: a climbable perch (verticality) ─────────────────────
function makePlatform(rng: () => number, cx: number, cz: number, w: number, height = PLATFORM_H): Box[] {
  void rng;
  const x0 = snap(cx - w / 2), x1 = snap(cx + w / 2);
  const z1 = snap(cz + w / 2); // platform back
  const top: Box = { x0, x1, y0: 0, y1: height, z0: snap(cz - w / 2), z1 };
  const steps: Box[] = [top];
  // staircase descending from the platform's -z face out into the street
  const nSteps = Math.round(height / STEP_RISE);
  let z = top.z0;
  for (let i = nSteps; i >= 1; i--) {
    const h = i * STEP_RISE;
    const sz1 = z;
    const sz0 = snap(z - STEP_RUN);
    steps.push({ x0, x1, y0: 0, y1: h, z0: sz0, z1: sz1 });
    z = sz0;
  }
  return steps;
}

/** Sniper tower: a 3m perch with a long stair run — high ground worth
 *  fighting for, still 3m under the perimeter top. */
function makeTower(rng: () => number, cx: number, cz: number, w: number): Box[] {
  return makePlatform(rng, cx, cz, w, TOWER_H);
}

/** Catwalk bridge: two stair platforms joined by an elevated deck. Walkable on
 *  top; the deck bottom clears head height, so the street UNDER it stays a
 *  lane — over/under play from plain boxes. */
function makeBridge(rng: () => number, cx: number, cz: number, w: number, span: number): Box[] {
  const half = span / 2 + w / 2;
  const towerA = makePlatform(rng, cx - half, cz, w);
  const towerB = makePlatform(rng, cx + half, cz, w);
  const deck: Box = {
    x0: snap(cx - half + w / 2),
    x1: snap(cx + half - w / 2),
    y0: DECK_BOTTOM,
    y1: DECK_TOP,
    z0: snap(cz - w / 2),
    z1: snap(cz + w / 2),
  };
  return [...towerA, ...towerB, deck];
}

/** L-shaped corner wall — street cover you can wrap around. */
function makeLWall(rng: () => number, cx: number, cz: number): Box[] {
  const t = 0.45;
  const lenA = snap(2.5 + rng() * 1.5);
  const lenB = snap(2.0 + rng() * 1.5);
  const h = 1.7 + rng() * 0.5;
  const x0 = snap(cx), z0 = snap(cz);
  return [
    { x0, x1: snap(x0 + lenA), y0: 0, y1: h, z0, z1: snap(z0 + t) },
    { x0, x1: snap(x0 + t), y0: 0, y1: h, z0, z1: snap(z0 + lenB) },
  ];
}

export function generateArena(seed: string): Arena {
  const rng = mulberry32(hashSeed(seed));
  const H = ARENA_HALF;
  const lim = H - EDGE_MARGIN;
  const cover: Box[] = [];
  const barrels: { x: number; z: number; wallIndex: number }[] = [];

  const footprintOf = (segs: Box[]): Box => ({
    x0: Math.min(...segs.map((s) => s.x0)),
    x1: Math.max(...segs.map((s) => s.x1)),
    y0: 0,
    y1: WALL_H,
    z0: Math.min(...segs.map((s) => s.z0)),
    z1: Math.max(...segs.map((s) => s.z1)),
  });
  const groupFits = (segs: Box[]): boolean => {
    const fp = footprintOf(segs);
    if (fp.x0 < -lim || fp.x1 > lim || fp.z0 < -lim || fp.z1 > lim) return false;
    if (nearSpawn(fp)) return false;
    return !cover.some((c) => overlapsXZ(inflate(fp, GAP), c));
  };
  const addMirrored = (segs: Box[]): void => {
    for (const piece of segs) cover.push(piece, mirror(piece));
  };
  const placeGroup = (make: () => Box[], tries: number): boolean => {
    for (let attempt = 0; attempt < tries; attempt++) {
      const g = make();
      const fp = footprintOf(g);
      if (groupFits(g) && !overlapsXZ(inflate(fp, GAP), mirror(fp))) {
        addMirrored(g);
        return true;
      }
    }
    return false;
  };

  // ── 1) buildings — the city blocks (3–5 pairs; bigger footprints can roll
  //       two-room interiors and tall parapet variants)
  const buildingPairs = 3 + Math.floor(rng() * 3);
  for (let n = 0; n < buildingPairs; n++) {
    placeGroup(() => {
      const w = 5 + rng() * 4;
      const d = 5 + rng() * 4;
      const cx = (rng() * 2 - 1) * (lim - w / 2 - 0.6);
      const cz = -(4 + rng() * (lim - d / 2 - 4.5));
      return makeBuilding(rng, cx, cz, w, d);
    }, 40);
  }

  // ── 2) the high ground: platforms, a catwalk bridge, a sniper tower
  const platformPairs = 1 + (rng() < 0.6 ? 1 : 0);
  for (let n = 0; n < platformPairs; n++) {
    placeGroup(() => {
      const w = 3.5 + rng() * 1.5;
      const cx = (rng() * 2 - 1) * (lim - w - 1.5);
      const cz = -(5 + rng() * (lim - w - 6));
      return makePlatform(rng, cx, cz, w);
    }, 36);
  }
  if (rng() < 0.7) {
    placeGroup(() => {
      const w = 3 + rng();
      const span = 3 + rng() * 2;
      const need = w + span / 2 + 1.5;
      const cx = (rng() * 2 - 1) * (lim - need);
      const cz = -(6 + rng() * (lim - w - 7));
      return makeBridge(rng, cx, cz, w, span);
    }, 36);
  }
  if (rng() < 0.7) {
    placeGroup(() => {
      const w = 3 + rng() * 0.5;
      const cx = (rng() * 2 - 1) * (lim - w - 1.5);
      // stairs run ~4.8m toward -z — keep the whole run inside bounds
      const cz = -(8 + rng() * (lim - w - 9));
      return makeTower(rng, cx, cz, w);
    }, 36);
  }

  // ── 2b) L-shaped corner walls (1–2 pairs of wrap-around street cover)
  const lWalls = 1 + (rng() < 0.5 ? 1 : 0);
  for (let n = 0; n < lWalls; n++) {
    placeGroup(() => {
      const cx = (rng() * 2 - 1) * (lim - 5);
      const cz = -(2 + rng() * (lim - 7));
      return makeLWall(rng, cx, cz);
    }, 30);
  }

  // ── 3) optional centerpiece
  if (rng() < 0.5) {
    const w = 1.4 + rng() * 1.4;
    const d = 1.4 + rng() * 1.4;
    const h = 1.6 + rng() * 1.0;
    const c: Box = { x0: snap(-w), x1: snap(w), y0: 0, y1: h, z0: snap(-d), z1: snap(d) };
    if (groupFits([c])) cover.push(c);
  }

  // ── 4) street furniture — crates / barriers / pillars / barrels
  const target = 9 + Math.floor(rng() * 6);
  let placed = 0;
  for (let attempt = 0; attempt < 220 && placed < target; attempt++) {
    const kind = rng();
    let w: number, d: number, h: number, barrel = false;
    if (kind < 0.34) {
      w = 0.8 + rng() * 1.0; d = 0.8 + rng() * 1.0; h = 1.0 + rng() * 1.2; // crate (climbable if low)
    } else if (kind < 0.62) {
      const long = 1.8 + rng() * 1.6; const flip = rng() < 0.5; // barrier
      w = flip ? long : 0.45; d = flip ? 0.45 : long; h = 1.5 + rng() * 0.9;
    } else if (kind < 0.82) {
      w = 0.6; d = 0.6; h = 1.6 + rng() * 1.0; // pillar
    } else {
      w = 0.55; d = 0.55; h = 1.1; barrel = true; // EXPLOSIVE barrel (rendered round)
    }
    const cx = snap((rng() * 2 - 1) * (lim - w));
    const cz = snap(-(0.8 + rng() * (lim - d - 0.8)));
    // no spawn-traps: a barrel too close to a spawn becomes a plain crate
    // (the spawn set is symmetric, so this covers the mirrored twin too)
    if (barrel && SPAWNS.some((s) => Math.hypot(cx - s.x, cz - s.z) < BARREL_SPAWN_CLEAR)) {
      barrel = false;
    }
    const piece: Box = { x0: cx - w, x1: cx + w, y0: 0, y1: h, z0: cz - d, z1: cz + d };
    const twin = mirror(piece);
    if (groupFits([piece]) && !overlapsXZ(inflate(piece, GAP), twin) && groupFits([twin])) {
      const coverIdx = cover.length; // addMirrored pushes piece, then twin
      addMirrored([piece]);
      if (barrel) {
        // walls = 4 perimeter boxes + cover, so wallIndex = 4 + cover index
        barrels.push(
          { x: cx, z: cz, wallIndex: 4 + coverIdx },
          { x: -cx, z: -cz, wallIndex: 4 + coverIdx + 1 },
        );
      }
      placed++;
    }
  }

  // ── 5) fairness: spawn-to-spawn must NOT be open sight
  const a = SPAWNS[0], b = SPAWNS[1];
  const dx = b.x - a.x, dz = b.z - a.z, dist = Math.hypot(dx, dz);
  const t = nearestWallT({ x: a.x, y: PLAYER_EYE, z: a.z }, { x: dx / dist, y: 0, z: dz / dist }, cover);
  if (t === null || t > dist) {
    cover.push({ x0: -2, x1: 2, y0: 0, y1: 2.0, z0: -2, z1: 2 });
  }

  // ── 6) three mirrored pickup pairs (bigger map → more loot)
  const walls = [...perimeter(), ...cover];
  const arenaSoFar: Arena = { seed, walls, spawns: SPAWNS, pickups: [], accent: 0, barrels };
  const avoid = SPAWNS.map((s) => ({ x: s.x, z: s.z, r: 5 }));
  const p1 = findOpenSpot(arenaSoFar, rng, avoid);
  const p2 = findOpenSpot(arenaSoFar, rng, [...avoid, { x: p1.x, z: p1.z, r: 8 }, { x: -p1.x, z: -p1.z, r: 8 }]);
  const p3 = findOpenSpot(arenaSoFar, rng, [
    ...avoid,
    { x: p1.x, z: p1.z, r: 8 }, { x: -p1.x, z: -p1.z, r: 8 },
    { x: p2.x, z: p2.z, r: 8 }, { x: -p2.x, z: -p2.z, r: 8 },
  ]);
  const pickups = [p1, { x: -p1.x, z: -p1.z }, p2, { x: -p2.x, z: -p2.z }, p3, { x: -p3.x, z: -p3.z }];

  return {
    seed,
    walls,
    spawns: SPAWNS,
    pickups,
    accent: ACCENTS[Math.floor(rng() * ACCENTS.length)],
    barrels,
  };
}

/** A clear standing spot on the GROUND — deterministic given rng. */
export function findOpenSpot(
  arena: Arena,
  rng: () => number,
  avoid: { x: number; z: number; r: number }[] = [],
): { x: number; z: number } {
  const lim = ARENA_HALF - 1.5;
  for (let i = 0; i < 120; i++) {
    const x = (rng() * 2 - 1) * lim;
    const z = (rng() * 2 - 1) * lim;
    const probe: Box = { x0: x - 0.6, x1: x + 0.6, y0: 0, y1: 1.8, z0: z - 0.6, z1: z + 0.6 };
    if (arena.walls.some((w) => w.y0 < 1.0 && overlapsXZ(inflate(probe, 0.4), w))) continue;
    if (avoid.some((s) => (s.x - x) ** 2 + (s.z - z) ** 2 < s.r * s.r)) continue;
    return { x, z };
  }
  return { x: 0, z: -10 };
}

/** Deterministic rng from a string seed — client decoration that should match
 *  per map (clouds etc.) without touching gameplay. */
export function seededRandom(seed: string): () => number {
  return mulberry32(hashSeed(seed));
}

export function randomSeed(): string {
  return `m${Date.now().toString(36)}${Math.floor(Math.random() * 1e9).toString(36)}`;
}
