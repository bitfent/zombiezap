// Minimal ray math for hitscan: ray vs AABB (wall occlusion) and ray vs
// sphere (player hitboxes, practice targets). Shared so the practice mode's
// client-side hitscan behaves exactly like the server's authoritative one.

import type { Box } from "./arena.ts";

export interface Vec3 {
  x: number;
  y: number;
  z: number;
}

/** Forward vector from yaw/pitch (three.js 'YXZ' convention: -Z is forward). */
export function dirFromAngles(yaw: number, pitch: number): Vec3 {
  const cp = Math.cos(pitch);
  return { x: -Math.sin(yaw) * cp, y: Math.sin(pitch), z: -Math.cos(yaw) * cp };
}

/** Slab test. Returns distance t >= 0 to entry, or null if no hit. */
export function rayBox(o: Vec3, d: Vec3, b: Box): number | null {
  let tmin = -Infinity;
  let tmax = Infinity;
  const axes: ["x" | "y" | "z", number, number][] = [
    ["x", b.x0, b.x1],
    ["y", b.y0, b.y1],
    ["z", b.z0, b.z1],
  ];
  for (const [axis, lo, hi] of axes) {
    const dv = d[axis];
    const ov = o[axis];
    if (Math.abs(dv) < 1e-9) {
      if (ov < lo || ov > hi) return null;
      continue;
    }
    let t1 = (lo - ov) / dv;
    let t2 = (hi - ov) / dv;
    if (t1 > t2) [t1, t2] = [t2, t1];
    tmin = Math.max(tmin, t1);
    tmax = Math.min(tmax, t2);
    if (tmin > tmax) return null;
  }
  if (tmax < 0) return null;
  return Math.max(tmin, 0);
}

/** Returns distance t >= 0 to the sphere, or null. */
export function raySphere(o: Vec3, d: Vec3, c: Vec3, r: number): number | null {
  const ox = o.x - c.x;
  const oy = o.y - c.y;
  const oz = o.z - c.z;
  const b = ox * d.x + oy * d.y + oz * d.z;
  const cc = ox * ox + oy * oy + oz * oz - r * r;
  const disc = b * b - cc;
  if (disc < 0) return null;
  const t = -b - Math.sqrt(disc);
  if (t < 0) return cc <= 0 ? 0 : null; // inside or behind
  return t;
}

/** Nearest wall hit along a ray, or null. */
export function nearestWallT(o: Vec3, d: Vec3, walls: Box[]): number | null {
  return nearestWallHit(o, d, walls)?.t ?? null;
}

/** Like nearestWallT but also says WHICH wall stopped the ray — explosive
 *  barrels need to know the shot ended on them. */
export function nearestWallHit(o: Vec3, d: Vec3, walls: Box[]): { t: number; index: number } | null {
  let best: { t: number; index: number } | null = null;
  for (let i = 0; i < walls.length; i++) {
    const t = rayBox(o, d, walls[i]);
    if (t !== null && (best === null || t < best.t)) best = { t, index: i };
  }
  return best;
}
