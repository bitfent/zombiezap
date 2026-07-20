#!/usr/bin/env node
/**
 * Rome EUR OSM → zz-core fixture baker.
 *
 * Reads the committed Overpass extract (rome-corridor.json), projects to
 * metres, decomposes building polygons on a 2 m grid with greedy rectangle
 * cover, anchors a 500×500 m window on the basilica, authors the hollow
 * basilica interior + street cover, and emits crates/zz-core/src/map/rome_eur.rs
 * with deterministic, byte-stable formatting.
 *
 * Zero network — all input is the committed JSON. Map data © OpenStreetMap
 * contributors, ODbL.
 *
 * Usage: node scripts/rome-eur/bake.mjs
 */

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(__dirname, "../..");
const INPUT = path.join(__dirname, "rome-corridor.json");
const OUTPUT = path.join(ROOT, "crates/zz-core/src/map/rome_eur.rs");

const BASILICA_WAY = 23432370;
const HALF = 250.0;
const CELL = 2.0;
const WALL_T = 1.2;
const PORTAL_W = 4.0;
const ROOF_Y = 14.0;
const ROOF_T = 0.5;
const DEFAULT_H = 9.0;
const MIN_AREA = 1.0; // m² after clip
const COVER_SPACING = 25.0;

// ── load ──────────────────────────────────────────────────────────────────
const data = JSON.parse(fs.readFileSync(INPUT, "utf8"));
const els = data.elements;
const streets = els.filter(
  (e) =>
    e.tags?.highway &&
    e.tags.highway !== "pedestrian" &&
    !e.tags.building
);
const builds = els.filter((e) => e.tags?.building);

// ── projection (street-centroid lat/lon, then basilica-anchor) ────────────
const streetPts = streets.flatMap((e) => e.geometry || []);
if (!streetPts.length) {
  console.error("no street geometry in extract");
  process.exit(1);
}
const lat0 = streetPts.reduce((s, p) => s + p.lat, 0) / streetPts.length;
const lon0 = streetPts.reduce((s, p) => s + p.lon, 0) / streetPts.length;
const MLAT = 111320.0;
const MLON = 111320.0 * Math.cos((lat0 * Math.PI) / 180);

function lonlatToMeters(p) {
  // x east, z south (screen-friendly)
  return [(p.lon - lon0) * MLON, -(p.lat - lat0) * MLAT];
}

const basEl = builds.find((e) => e.id === BASILICA_WAY);
if (!basEl) {
  console.error("basilica way missing from extract");
  process.exit(1);
}
const basPolyRaw = (basEl.geometry || []).slice(0, -1).map(lonlatToMeters);
if (basPolyRaw.length < 3) {
  console.error("basilica polygon degenerate");
  process.exit(1);
}
const basCx =
  basPolyRaw.reduce((s, p) => s + p[0], 0) / basPolyRaw.length;
const basCz =
  basPolyRaw.reduce((s, p) => s + p[1], 0) / basPolyRaw.length;

function project(p) {
  const [x, z] = lonlatToMeters(p);
  return [x - basCx, z - basCz];
}

// ── geometry helpers ──────────────────────────────────────────────────────
function parseHeight(tags) {
  const h = tags?.height;
  if (h != null) {
    const v = parseFloat(String(h).replace(/m/gi, "").trim());
    if (Number.isFinite(v) && v > 0) return v;
  }
  const lv = tags?.["building:levels"];
  if (lv != null) {
    const v = parseFloat(lv);
    if (Number.isFinite(v) && v > 0) return v * 3.2 + 1.0;
  }
  return DEFAULT_H;
}

function pointInPoly(x, z, poly) {
  let inside = false;
  const n = poly.length;
  for (let i = 0; i < n; i++) {
    const [x1, z1] = poly[i];
    const [x2, z2] = poly[(i + 1) % n];
    if (z1 > z !== z2 > z) {
      const t = (z - z1) / (z2 - z1);
      if (x < x1 + t * (x2 - x1)) inside = !inside;
    }
  }
  return inside;
}

/** 2 m grid + greedy rectangle cover → list of [x0,z0,x1,z1]. */
function decompose(poly, cell = CELL) {
  const xs = poly.map((p) => p[0]);
  const zs = poly.map((p) => p[1]);
  let x0 = Math.min(...xs);
  let x1 = Math.max(...xs);
  let z0 = Math.min(...zs);
  let z1 = Math.max(...zs);
  const nx = Math.max(1, Math.ceil((x1 - x0) / cell));
  const nz = Math.max(1, Math.ceil((z1 - z0) / cell));
  if (nx * nz > 200_000) return [[x0, z0, x1, z1]];
  const grid = Array.from({ length: nz }, (_, j) =>
    Array.from({ length: nx }, (_, i) =>
      pointInPoly(x0 + (i + 0.5) * cell, z0 + (j + 0.5) * cell, poly)
    )
  );
  const rects = [];
  for (let j = 0; j < nz; j++) {
    for (let i = 0; i < nx; i++) {
      if (!grid[j][i]) continue;
      let w = 1;
      while (i + w < nx && grid[j][i + w]) w++;
      let h = 1;
      while (j + h < nz && grid[j + h].slice(i, i + w).every(Boolean)) h++;
      for (let jj = j; jj < j + h; jj++) {
        for (let ii = i; ii < i + w; ii++) grid[jj][ii] = false;
      }
      rects.push([
        x0 + i * cell,
        z0 + j * cell,
        x0 + (i + w) * cell,
        z0 + (j + h) * cell,
      ]);
    }
  }
  return rects;
}

function clipRect(r) {
  const x0 = Math.max(r[0], -HALF);
  const z0 = Math.max(r[1], -HALF);
  const x1 = Math.min(r[2], HALF);
  const z1 = Math.min(r[3], HALF);
  if (x1 - x0 < 0.5 || z1 - z0 < 0.5) return null;
  if ((x1 - x0) * (z1 - z0) < MIN_AREA) return null;
  return [x0, z0, x1, z1];
}

function round1(v) {
  // one decimal, stable (avoid -0)
  const r = Math.round(v * 10) / 10;
  return Object.is(r, -0) ? 0 : r;
}

function fmt(v) {
  const r = round1(v);
  // always one decimal for byte-stability
  return r.toFixed(1);
}

/** Emit yaw literals that clippy accepts (no bare π approximations). */
function fmtYaw(v) {
  const pi = Math.PI;
  if (Math.abs(v - pi) < 1e-4) return "core::f32::consts::PI";
  if (Math.abs(v + pi) < 1e-4) return "-core::f32::consts::PI";
  if (Math.abs(v - pi / 2) < 1e-4) return "core::f32::consts::FRAC_PI_2";
  if (Math.abs(v + pi / 2) < 1e-4) return "-core::f32::consts::FRAC_PI_2";
  if (Math.abs(v - pi / 4) < 1e-4) return "core::f32::consts::FRAC_PI_4";
  if (Math.abs(v + pi / 4) < 1e-4) return "-core::f32::consts::FRAC_PI_4";
  if (Math.abs(v - (pi - pi / 4)) < 1e-4) {
    return "core::f32::consts::PI - core::f32::consts::FRAC_PI_4";
  }
  if (Math.abs(v - (-pi + pi / 4)) < 1e-4) {
    return "-core::f32::consts::PI + core::f32::consts::FRAC_PI_4";
  }
  // generic: six decimals is enough for facing; allow clippy via typed const
  return `${v.toFixed(6)}_f32`;
}

// ── buildings (non-basilica solid extrusions) ─────────────────────────────
/** @type {{x0:number,x1:number,y0:number,y1:number,z0:number,z1:number}[]} */
const walls = [];
/** large facade candidates for billboard placement (baker-side metadata) */
const facades = [];

const basPoly = basPolyRaw.map(([x, z]) => [x - basCx, z - basCz]);
const basXs = basPoly.map((p) => p[0]);
const basZs = basPoly.map((p) => p[1]);
const basMinX = Math.min(...basXs);
const basMaxX = Math.max(...basXs);
const basMinZ = Math.min(...basZs);
const basMaxZ = Math.max(...basZs);

for (const e of builds) {
  if (e.id === BASILICA_WAY) continue;
  const poly = (e.geometry || []).slice(0, -1).map(project);
  if (poly.length < 3) continue;
  const h = Math.min(parseHeight(e.tags || {}), 40.0); // cap skyline
  const rects = decompose(poly);
  for (const r of rects) {
    const c = clipRect(r);
    if (!c) continue;
    const [x0, z0, x1, z1] = c;
    walls.push({ x0, x1, y0: 0, y1: h, z0, z1 });
    const area = (x1 - x0) * (z1 - z0);
    if (area >= 80) {
      facades.push({ x0, x1, z0, z1, h });
    }
  }
}

// ── basilica: hollow perimeter + portal + roof + dome + columns ───────────
// North = −z is the open piazzale (no buildings). Portal opens that way.
function pushWall(x0, x1, y0, y1, z0, z1) {
  const c = clipRect([x0, z0, x1, z1]);
  if (!c) return;
  // y not clipped to half — vertical free
  walls.push({
    x0: c[0],
    x1: c[2],
    y0,
    y1,
    z0: c[1],
    z1: c[3],
  });
}

// Perimeter walls: extrude each polygon edge as a WALL_T-thick prism,
// skipping a PORTAL_W opening on the most-north-facing long edge.
function edgeOutwardMid(a, b) {
  const mx = (a[0] + b[0]) / 2;
  const mz = (a[1] + b[1]) / 2;
  const dx = b[0] - a[0];
  const dz = b[1] - a[1];
  const len = Math.hypot(dx, dz) || 1;
  // inward normal candidates (left/right of directed edge); pick the one
  // pointing toward polygon centroid (origin ≈ basilica center).
  let nx = -dz / len;
  let nz = dx / len;
  // if outward (away from origin), flip
  if (mx * nx + mz * nz > 0) {
    nx = -nx;
    nz = -nz;
  }
  // we want OUTWARD for portal detection: opposite of inward
  return { mx, mz, len, ox: -nx, oz: -nz, dx, dz, ux: dx / len, uz: dz / len };
}

// Pick portal edge: longest edge whose outward normal points most −z (north).
let portalEdge = -1;
let portalScore = -Infinity;
const edgeMeta = [];
for (let i = 0; i < basPoly.length; i++) {
  const a = basPoly[i];
  const b = basPoly[(i + 1) % basPoly.length];
  const m = edgeOutwardMid(a, b);
  edgeMeta.push(m);
  // prefer north-facing (oz < 0) long edges
  const score = m.len * 0.1 + (m.oz < -0.3 ? -m.oz * m.len : -100);
  if (score > portalScore) {
    portalScore = score;
    portalEdge = i;
  }
}

const WALL_H_BAS = ROOF_Y; // walls meet the roof slab

for (let i = 0; i < basPoly.length; i++) {
  const a = basPoly[i];
  const b = basPoly[(i + 1) % basPoly.length];
  const m = edgeMeta[i];
  if (m.len < 0.5) continue;

  // offset outward by WALL_T/2 so the wall straddles the outline
  const ox = m.ox * (WALL_T / 2);
  const oz = m.oz * (WALL_T / 2);
  // also expand along the edge slightly so corners meet
  const pad = WALL_T / 2;

  if (i === portalEdge) {
    // leave PORTAL_W gap centered on the edge
    const halfGap = PORTAL_W / 2;
    const total = m.len;
    if (total <= PORTAL_W + 1.0) {
      // edge too short — skip wall entirely (rare)
      continue;
    }
    // two segments around the portal
    const segs = [
      { t0: 0, t1: total / 2 - halfGap },
      { t0: total / 2 + halfGap, t1: total },
    ];
    for (const s of segs) {
      if (s.t1 - s.t0 < 0.4) continue;
      const p0x = a[0] + m.ux * s.t0;
      const p0z = a[1] + m.uz * s.t0;
      const p1x = a[0] + m.ux * s.t1;
      const p1z = a[1] + m.uz * s.t1;
      const xs = [p0x + ox, p1x + ox, p0x - ox, p1x - ox];
      const zs = [p0z + oz, p1z + oz, p0z - oz, p1z - oz];
      // expand AABB along edge by pad at ends for the outer corners only
      pushWall(
        Math.min(...xs) - 0.01,
        Math.max(...xs) + 0.01,
        0,
        WALL_H_BAS,
        Math.min(...zs) - 0.01,
        Math.max(...zs) + 0.01
      );
    }
    // lintel above portal
    {
      const t0 = total / 2 - halfGap;
      const t1 = total / 2 + halfGap;
      const p0x = a[0] + m.ux * t0;
      const p0z = a[1] + m.uz * t0;
      const p1x = a[0] + m.ux * t1;
      const p1z = a[1] + m.uz * t1;
      const xs = [p0x + ox, p1x + ox, p0x - ox, p1x - ox];
      const zs = [p0z + oz, p1z + oz, p0z - oz, p1z - oz];
      pushWall(
        Math.min(...xs),
        Math.max(...xs),
        3.2,
        WALL_H_BAS,
        Math.min(...zs),
        Math.max(...zs)
      );
    }
  } else {
    // full edge wall as AABB covering the thick band around the edge
    const p0x = a[0] - m.ux * pad;
    const p0z = a[1] - m.uz * pad;
    const p1x = b[0] + m.ux * pad;
    const p1z = b[1] + m.uz * pad;
    const xs = [p0x + ox, p1x + ox, p0x - ox, p1x - ox];
    const zs = [p0z + oz, p1z + oz, p0z - oz, p1z - oz];
    pushWall(
      Math.min(...xs),
      Math.max(...xs),
      0,
      WALL_H_BAS,
      Math.min(...zs),
      Math.max(...zs)
    );
  }
}

// Roof slab over footprint AABB (slightly inset so it sits on walls)
{
  const inset = 0.3;
  pushWall(
    basMinX + inset,
    basMaxX - inset,
    ROOF_Y,
    ROOF_Y + ROOF_T,
    basMinZ + inset,
    basMaxZ - inset
  );
}

// Dome tiers: 2–3 stacked shrinking boxes on the roof (stylized, not 88 m)
{
  const cx = 0;
  const cz = 0;
  const tiers = [
    { half: 10.0, y0: ROOF_Y + ROOF_T, y1: ROOF_Y + ROOF_T + 4.0 },
    { half: 7.0, y0: ROOF_Y + ROOF_T + 4.0, y1: ROOF_Y + ROOF_T + 7.5 },
    { half: 4.0, y0: ROOF_Y + ROOF_T + 7.5, y1: ROOF_Y + ROOF_T + 10.5 },
  ];
  for (const t of tiers) {
    pushWall(cx - t.half, cx + t.half, t.y0, t.y1, cz - t.half, cz + t.half);
  }
}

// Interior columns: 2 rows of ~1×1 m pillars, ≥3 m aisles
{
  const col = 1.0;
  const halfCol = col / 2;
  // nave roughly along x (east-west); rows offset in z
  const rowZ = [-8.0, 8.0];
  const colXs = [-12.0, -6.0, 0.0, 6.0, 12.0];
  for (const z of rowZ) {
    for (const x of colXs) {
      // keep clear of portal (north side center)
      if (z < -5 && Math.abs(x) < 3) continue;
      pushWall(
        x - halfCol,
        x + halfCol,
        0,
        ROOF_Y - 0.2,
        z - halfCol,
        z + halfCol
      );
    }
  }
}

// Optional low altar block on the south end (opposite portal)
pushWall(-2.0, 2.0, 0, 1.0, basMaxZ - 6.0, basMaxZ - 4.0);

// Portal reference (mid of portal edge, just outside) for spawns / yaw
const pe = edgeMeta[portalEdge];
const portalX = pe.mx + pe.ox * 2.0;
const portalZ = pe.mz + pe.oz * 2.0;
// yaw facing into the basilica from the piazzale (toward center from portal)
const portalYaw = Math.atan2(-(0 - portalX), -(0 - portalZ)); // face origin
// atan2(-dx, -dz) with facing convention: yaw 0 faces −z
// survivors face into the building: from portal toward +oz direction flipped
// Our movement yaw: 0 faces −z (north). From piazzale (north of portal edge)
// looking into the church is roughly facing +z (south).
const spawnYaw = Math.PI; // face +z (into the arena/streets / church entrance)

// ── street polylines (for cover + gates) ──────────────────────────────────
const streetPolys = streets.map((e) =>
  (e.geometry || []).map(project)
);

/** Segment-AABB edge intersections for gate placement. */
function segmentEdgeHits(x0, z0, x1, z1) {
  const hits = [];
  const edges = [
    // z = ±HALF (south/north), x in range
    { axis: "z", fixed: HALF, a0: x0, a1: x1, b0: z0, b1: z1, vary: "x" },
    { axis: "z", fixed: -HALF, a0: x0, a1: x1, b0: z0, b1: z1, vary: "x" },
    { axis: "x", fixed: HALF, a0: z0, a1: z1, b0: x0, b1: x1, vary: "z" },
    { axis: "x", fixed: -HALF, a0: z0, a1: z1, b0: x0, b1: x1, vary: "z" },
  ];
  // parametric line vs constant axis
  function hitConst(constAxis, constVal, p0a, p0b, p1a, p1b) {
    // constAxis is x or z held at constVal; a is the other coord
    const d = p1b - p0b;
    if (Math.abs(d) < 1e-9) return null;
    const t = (constVal - p0b) / d;
    if (t < -0.01 || t > 1.01) return null;
    const a = p0a + t * (p1a - p0a);
    if (a < -HALF - 1 || a > HALF + 1) return null;
    if (constAxis === "z") return { x: a, z: constVal };
    return { x: constVal, z: a };
  }
  const h1 = hitConst("z", HALF, x0, z0, x1, z1);
  const h2 = hitConst("z", -HALF, x0, z0, x1, z1);
  const h3 = hitConst("x", HALF, z0, x0, z1, x1);
  const h4 = hitConst("x", -HALF, z0, x0, z1, x1);
  for (const h of [h1, h2, h3, h4]) {
    if (h) hits.push(h);
  }
  return hits;
}

const rawGateHits = [];
for (const poly of streetPolys) {
  for (let i = 0; i < poly.length - 1; i++) {
    const [x0, z0] = poly[i];
    const [x1, z1] = poly[i + 1];
    for (const h of segmentEdgeHits(x0, z0, x1, z1)) {
      // clamp onto the playable edge (inset)
      const inset = 1.2;
      let { x, z } = h;
      x = Math.max(-HALF + inset, Math.min(HALF - inset, x));
      z = Math.max(-HALF + inset, Math.min(HALF - inset, z));
      rawGateHits.push({ x, z });
    }
  }
}

/** Cluster gate hits within 30 m → unique gates. */
function clusterGates(hits, mergeR = 30) {
  const out = [];
  for (const h of hits) {
    let merged = false;
    for (const g of out) {
      const d = Math.hypot(g.x - h.x, g.z - h.z);
      if (d < mergeR) {
        g.x = (g.x * g.n + h.x) / (g.n + 1);
        g.z = (g.z * g.n + h.z) / (g.n + 1);
        g.n += 1;
        merged = true;
        break;
      }
    }
    if (!merged) out.push({ x: h.x, z: h.z, n: 1 });
  }
  return out;
}

let gateCenters = clusterGates(rawGateHits);

// Ensure ≥4 gates: if streets only give a few exits, add the open
// mid-edge candidates that sit on walkable corridor approaches.
function yawTowardCenter(x, z) {
  // yaw 0 faces −z; face toward origin
  return Math.atan2(-x, -z);
}

const fallbackGates = [
  { x: 0, z: HALF - 1.2 },
  { x: 0, z: -HALF + 1.2 },
  { x: HALF - 1.2, z: 0 },
  { x: -HALF + 1.2, z: 0 },
  { x: HALF - 1.2, z: HALF - 1.2 },
  { x: -HALF + 1.2, z: HALF - 1.2 },
];

// Prefer street-derived; fill to 4 with fallbacks far from existing
for (const f of fallbackGates) {
  if (gateCenters.length >= 4) break;
  if (gateCenters.some((g) => Math.hypot(g.x - f.x, g.z - f.z) < 40)) continue;
  gateCenters.push({ x: f.x, z: f.z, n: 0 });
}
gateCenters = gateCenters.slice(0, 6);

const gates = gateCenters.map((g) => ({
  x: round1(g.x),
  z: round1(g.z),
  yaw: yawTowardCenter(g.x, g.z),
}));

// ── street cover: cars + crates along boulevard samples every ~25 m ───────
function pointBlockedByBuilding(x, z, margin = 2.5) {
  for (const w of walls) {
    if (w.y1 < 1.5) continue; // ignore low cover already placed
    if (
      x > w.x0 - margin &&
      x < w.x1 + margin &&
      z > w.z0 - margin &&
      z < w.z1 + margin
    ) {
      // only treat as building mass if tall
      if (w.y1 >= 2.5) return true;
    }
  }
  return false;
}

function nearSpawn(x, z, sx, sz, clear = 8) {
  return (x - sx) * (x - sx) + (z - sz) * (z - sz) < clear * clear;
}

// Spawn cluster on piazzale: north of portal (more −z)
const spawnAnchorX = round1(portalX);
const spawnAnchorZ = round1(portalZ - 12.0); // further into piazzale
// Keep inside arena
const ax = Math.max(-HALF + 5, Math.min(HALF - 5, spawnAnchorX));
const az = Math.max(-HALF + 5, Math.min(HALF - 5, spawnAnchorZ));

const coverBoxes = [];
const placedCover = [];

function addCover(x0, x1, y0, y1, z0, z1) {
  const c = clipRect([x0, z0, x1, z1]);
  if (!c) return false;
  const box = {
    x0: c[0],
    x1: c[2],
    y0,
    y1,
    z0: c[1],
    z1: c[3],
  };
  // no overlap with existing cover
  for (const o of placedCover) {
    if (box.x0 < o.x1 && box.x1 > o.x0 && box.z0 < o.z1 && box.z1 > o.z0) {
      return false;
    }
  }
  // keep BFS: never block the full street — only place if center is free
  // and not on spawn
  const cx = (box.x0 + box.x1) / 2;
  const cz = (box.z0 + box.z1) / 2;
  if (nearSpawn(cx, cz, ax, az, 10)) return false;
  if (pointBlockedByBuilding(cx, cz, 1.5)) return false;
  // keep away from basilica interior
  if (
    cx > basMinX - 2 &&
    cx < basMaxX + 2 &&
    cz > basMinZ - 2 &&
    cz < basMaxZ + 2
  ) {
    return false;
  }
  placedCover.push(box);
  coverBoxes.push(box);
  return true;
}

// Sample along each street polyline at COVER_SPACING
for (const poly of streetPolys) {
  let dist = 0;
  let nextAt = COVER_SPACING * 0.5;
  for (let i = 0; i < poly.length - 1; i++) {
    const [x0, z0] = poly[i];
    const [x1, z1] = poly[i + 1];
    const segLen = Math.hypot(x1 - x0, z1 - z0);
    if (segLen < 0.1) continue;
    const ux = (x1 - x0) / segLen;
    const uz = (z1 - z0) / segLen;
    // perpendicular for roadside offset
    const px = -uz;
    const pz = ux;
    let local = 0;
    while (local < segLen) {
      const abs = dist + local;
      if (abs >= nextAt) {
        const x = x0 + ux * local;
        const z = z0 + uz * local;
        if (Math.abs(x) < HALF - 3 && Math.abs(z) < HALF - 3) {
          // alternate car / crate, alternate side
          const k = Math.floor(nextAt / COVER_SPACING);
          const side = k % 2 === 0 ? 1 : -1;
          const offset = 4.0 * side; // roadside
          const cx = x + px * offset;
          const cz = z + pz * offset;
          if (k % 3 === 0) {
            // car-ish 2×1×1.2
            const alongX = Math.abs(ux) > Math.abs(uz);
            if (alongX) {
              addCover(cx - 1.0, cx + 1.0, 0, 1.2, cz - 0.5, cz + 0.5);
            } else {
              addCover(cx - 0.5, cx + 0.5, 0, 1.2, cz - 1.0, cz + 1.0);
            }
          } else {
            // crate 1×1×1
            addCover(cx - 0.5, cx + 0.5, 0, 1.0, cz - 0.5, cz + 0.5);
          }
        }
        nextAt += COVER_SPACING;
      }
      local += 1.0; // step 1 m along segment
    }
    dist += segLen;
  }
}

// Extra sightline breakers on the piazzale approaches if sparse
for (const [dx, dz] of [
  [-15, -20],
  [15, -18],
  [-20, -35],
  [18, -30],
  [0, -45],
]) {
  addCover(ax + dx - 0.5, ax + dx + 0.5, 0, 1.0, az + dz - 0.5, az + dz + 0.5);
}

walls.push(...coverBoxes);

// ── perimeter walls (arena boundary) ──────────────────────────────────────
const PERIM_T = 0.6;
const PERIM_H = 6.0;
const h = HALF;
walls.push(
  { x0: -h - PERIM_T, x1: h + PERIM_T, y0: 0, y1: PERIM_H, z0: -h - PERIM_T, z1: -h },
  { x0: -h - PERIM_T, x1: h + PERIM_T, y0: 0, y1: PERIM_H, z0: h, z1: h + PERIM_H },
  { x0: -h - PERIM_T, x1: -h, y0: 0, y1: PERIM_H, z0: -h, z1: h },
  { x0: h, x1: h + PERIM_T, y0: 0, y1: PERIM_H, z0: -h, z1: h }
);

// fix second perimeter wall z1 (typo guard)
walls[walls.length - 3].z1 = h + PERIM_T;

// ── spawns: 5-cluster on piazzale ─────────────────────────────────────────
const SPAWN_YAW = Math.PI; // face +z (toward basilica / streets)
const spawnOffsets = [
  [0, 0],
  [1.5, 0],
  [-1.5, 0],
  [0.8, 1.5],
  [-0.8, 1.5],
];
const spawns = spawnOffsets.map(([dx, dz]) => ({
  x: round1(ax + dx),
  z: round1(az + dz),
  yaw: SPAWN_YAW,
}));

// ── pickups: open ground samples near piazzale / streets ──────────────────
const pickupCandidates = [
  [ax + 20, az + 5],
  [ax - 22, az + 8],
  [0, 40],
  [30, 80],
  [-40, 60],
  [10, 120],
];
const pickups = [];
for (const [px, pz] of pickupCandidates) {
  if (Math.abs(px) > HALF - 5 || Math.abs(pz) > HALF - 5) continue;
  if (pointBlockedByBuilding(px, pz, 1.0)) continue;
  pickups.push([round1(px), round1(pz)]);
  if (pickups.length >= 5) break;
}
while (pickups.length < 3) {
  pickups.push([round1(ax + pickups.length * 3), round1(az - 5)]);
}

// ── billboard slots on perimeter (facade-aware optional extras later) ─────
// Runtime places perimeter billboards from seed; fixture only stores fixed
// facade anchors the wrapper may use. We emit 6 perimeter-style slots.
const billboards = [];
{
  const bw = 6.0;
  const bh = 2.5;
  const by = 4.2;
  const slots = [
    { x: 0, z: -HALF + 0.05, wall: 0 },
    { x: 0, z: HALF - 0.05, wall: 1 },
    { x: -HALF + 0.05, z: 0, wall: 2 },
    { x: HALF - 0.05, z: 0, wall: 3 },
    { x: HALF * 0.4, z: -HALF + 0.05, wall: 0 },
    { x: -HALF * 0.4, z: HALF - 0.05, wall: 1 },
  ];
  slots.forEach((s, i) => {
    billboards.push({
      x: round1(s.x),
      y: by,
      z: round1(s.z),
      wall: s.wall,
      w: bw,
      h: bh,
      ad_slot: i,
    });
  });
}

// ── quantize all walls for byte-stable f32 literals ───────────────────────
function qWall(w) {
  return {
    x0: round1(w.x0),
    x1: round1(w.x1),
    y0: round1(w.y0),
    y1: round1(w.y1),
    z0: round1(w.z0),
    z1: round1(w.z1),
  };
}
const qWalls = walls
  .map(qWall)
  .filter((w) => w.x0 < w.x1 && w.y0 < w.y1 && w.z0 < w.z1);

// Sort for determinism: y0, x0, z0, x1, z1, y1
qWalls.sort((a, b) => {
  for (const k of ["y0", "x0", "z0", "x1", "z1", "y1"]) {
    if (a[k] !== b[k]) return a[k] - b[k];
  }
  return 0;
});

// ── emit Rust ─────────────────────────────────────────────────────────────
function aabbLit(w) {
  return `    Aabb { x0: ${fmt(w.x0)}, x1: ${fmt(w.x1)}, y0: ${fmt(w.y0)}, y1: ${fmt(w.y1)}, z0: ${fmt(w.z0)}, z1: ${fmt(w.z1)} },`;
}

const wallLines = qWalls.map(aabbLit).join("\n");

const spawnLines = spawns
  .map(
    (s) =>
      `    Spawn { x: ${fmt(s.x)}, z: ${fmt(s.z)}, yaw: ${fmtYaw(s.yaw)} },`
  )
  .join("\n");

const gateLines = gates
  .map(
    (g) =>
      `    Gate { x: ${fmt(g.x)}, z: ${fmt(g.z)}, yaw: ${fmtYaw(g.yaw)} },`
  )
  .join("\n");

const pickupLines = pickups
  .map(([x, z]) => `    (${fmt(x)}, ${fmt(z)}),`)
  .join("\n");

const bbLines = billboards
  .map(
    (b) =>
      `    Billboard { x: ${fmt(b.x)}, y: ${fmt(b.y)}, z: ${fmt(b.z)}, wall: ${b.wall}, w: ${fmt(b.w)}, h: ${fmt(b.h)}, ad_slot: ${b.ad_slot} },`
  )
  .join("\n");

const rust = `// GENERATED by scripts/rome-eur/bake.mjs — do not hand-edit.
// Map data © OpenStreetMap contributors, ODbL.
//
// Re-generate: node scripts/rome-eur/bake.mjs
// Source: scripts/rome-eur/rome-corridor.json (basilica-anchored 500×500 m
// window, 2 m greedy AABB decomposition, authored basilica interior + street
// cover). Runtime is data-free: generate() returns this fixture; seed only
// varies accent via the ACCENTS table.

use super::coop::{Billboard, GameMap, Gate};
use super::ACCENTS;
use crate::types::{Aabb, EnvKind, Spawn};

/// Arena half-extent for Rome EUR (true-scale 500×500 m playfield).
pub const ARENA_HALF: f32 = 250.0;

/// Static collision AABBs (buildings, basilica shell, cover, perimeter).
pub static WALLS: &[Aabb] = &[
${wallLines}
];

/// Piazzale spawn cluster (5 slots) in front of the basilica portal.
pub static SPAWNS: &[Spawn] = &[
${spawnLines}
];

/// Street / edge exit gates (yaw faces arena center).
pub static GATES: &[Gate] = &[
${gateLines}
];

/// Fixed perimeter billboard anchors (ad_slot filled; seed may reshuffle copy).
pub static BILLBOARDS: &[Billboard] = &[
${bbLines}
];

/// Open-ground pickup / supply-drop anchors.
pub static PICKUPS: &[(f32, f32)] = &[
${pickupLines}
];

/// Assemble the co-op GameMap. \`seed\` selects accent only — geometry is fixed.
pub fn generate(seed: &str) -> GameMap {
    let accent_idx = {
        let mut h: u32 = 2166136261;
        for b in seed.as_bytes() {
            h ^= u32::from(*b);
            h = h.wrapping_mul(16777619);
        }
        (h as usize) % ACCENTS.len()
    };
    // Optional seed-stable ad_slot permutation (geometry fixed).
    let mut billboards: Vec<Billboard> = BILLBOARDS.to_vec();
    if seed != "rome-eur-fixed" {
        let mut rng = crate::rng::Mulberry32::from_seed(&format!("{seed}|rome-ads"));
        for b in &mut billboards {
            b.ad_slot = (rng.next() * 6.0).floor() as u8 % 6;
        }
    }
    GameMap {
        seed: seed.to_string(),
        env: EnvKind::RomeEur,
        arena_half: ARENA_HALF,
        walls: WALLS.to_vec(),
        spawns: SPAWNS.to_vec(),
        gates: GATES.to_vec(),
        billboards,
        pickups: PICKUPS.to_vec(),
        accent: ACCENTS[accent_idx],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::WalkGrid;
    use crate::constants::MAX_PLAYERS;

    #[test]
    fn fixture_invariants() {
        let map = generate("rome-test");
        assert_eq!(map.env, EnvKind::RomeEur);
        assert_eq!(map.arena_half, 250.0);
        assert_eq!(map.spawns.len(), MAX_PLAYERS);
        assert!(map.gates.len() >= 3);
        assert!(!map.walls.is_empty());
        assert!(map.walls.len() < 2000, "unexpectedly many AABBs");
        let grid = WalkGrid::rasterize(&map.walls, map.arena_half);
        for s in &map.spawns {
            assert!(
                grid.walkable_at(s.x, s.z),
                "spawn ({}, {}) not walkable",
                s.x,
                s.z
            );
        }
        let again = generate("rome-test");
        assert_eq!(map.walls, again.walls);
        assert_eq!(map.spawns, again.spawns);
        assert_eq!(map.gates, again.gates);
    }
}
`;

fs.writeFileSync(OUTPUT, rust);
console.log(
  JSON.stringify(
    {
      walls: qWalls.length,
      spawns: spawns.length,
      gates: gates.length,
      pickups: pickups.length,
      billboards: billboards.length,
      cover: coverBoxes.length,
      bas_portal: { x: round1(portalX), z: round1(portalZ), edge: portalEdge },
      spawn_anchor: { x: ax, z: az },
      output: path.relative(ROOT, OUTPUT),
    },
    null,
    2
  )
);
