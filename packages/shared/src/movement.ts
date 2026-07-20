// Deterministic movement with VERTICAL collision + step-up, shared by server
// simulation AND client prediction — the same function body runs on both
// sides, so the predicted camera and the authoritative position agree.
//
// Box rules (everything is an AABB):
//   * a box BLOCKS horizontal movement only if its top is more than STEP_UP
//     above your feet (a real wall) AND its bottom is below your head — so you
//     pass UNDER roofs/window-headers and step OVER low curbs/stairs
//   * the surface you stand on is the highest box-top under your feet that is
//     within STEP_UP of your current feet height — so you walk up stairs and
//     onto crates/platforms, and fall off ledges
// The perimeter walls are tall (WALL_H) and kept clear of climbable geometry,
// so the arena can't be escaped.

import { ARENA_HALF, type Box, type Spawn } from "./arena.ts";
import { GRAVITY, JUMP_VELOCITY, PLAYER_RADIUS, PLAYER_SPEED, STEP_UP } from "./constants.ts";
import type { PlayerInput } from "./types.ts";

export interface Body {
  x: number;
  y: number; // feet height (0 = ground)
  z: number;
  vy: number;
  onGround: boolean;
}

const HEAD = 1.7; // body clearance above feet (eye is 1.55)

function footprint(x: number, z: number, b: Box): boolean {
  return (
    x > b.x0 - PLAYER_RADIUS &&
    x < b.x1 + PLAYER_RADIUS &&
    z > b.z0 - PLAYER_RADIUS &&
    z < b.z1 + PLAYER_RADIUS
  );
}

/** Highest standable surface at (x,z), given current feet height. */
function groundAt(x: number, z: number, feetY: number, walls: Box[]): number {
  let g = 0;
  for (const b of walls) {
    if (b.y1 <= feetY + STEP_UP + 1e-3 && b.y1 > g && footprint(x, z, b)) g = b.y1;
  }
  return g;
}

/** Does a wall block horizontal movement into (x,z) at this feet height? */
function blockedXZ(x: number, z: number, feetY: number, walls: Box[]): boolean {
  const head = feetY + HEAD;
  for (const b of walls) {
    if (b.y1 <= feetY + STEP_UP + 1e-3) continue; // low — step over/onto it
    if (b.y0 >= head) continue; // high — pass under it
    if (footprint(x, z, b)) return true;
  }
  return false;
}

/** Advance one body by one input over dt seconds. Mutates and returns `b`. */
export function stepBody(b: Body, input: PlayerInput, dt: number, walls: Box[]): Body {
  let dx = 0;
  let dz = 0;
  const sin = Math.sin(input.yaw);
  const cos = Math.cos(input.yaw);
  if (input.forward) { dx -= sin; dz -= cos; }
  if (input.backward) { dx += sin; dz += cos; }
  if (input.left) { dx -= cos; dz += sin; }
  if (input.right) { dx += cos; dz -= sin; }
  const len = Math.hypot(dx, dz);
  if (len > 0) {
    dx = (dx / len) * PLAYER_SPEED * dt;
    dz = (dz / len) * PLAYER_SPEED * dt;
    if (!blockedXZ(b.x + dx, b.z, b.y, walls)) b.x += dx;
    if (!blockedXZ(b.x, b.z + dz, b.y, walls)) b.z += dz;
  }

  const ground = groundAt(b.x, b.z, b.y, walls);

  // step up onto a higher surface we just walked into (stairs, curbs, crates)
  if (b.onGround && ground > b.y && ground - b.y <= STEP_UP + 1e-3) {
    b.y = ground;
  }

  if (input.jump && b.onGround) {
    b.vy = JUMP_VELOCITY;
    b.onGround = false;
  }

  if (!b.onGround || b.vy !== 0) {
    b.y += b.vy * dt;
    b.vy -= GRAVITY * dt;
    if (b.y <= ground) {
      b.y = ground;
      b.vy = 0;
      b.onGround = true;
    }
  } else if (ground < b.y - 1e-3) {
    // walked off a ledge — start falling
    b.onGround = false;
  } else {
    b.y = ground;
  }

  // hard clamp inside the arena
  const lim = ARENA_HALF - PLAYER_RADIUS;
  b.x = Math.max(-lim, Math.min(lim, b.x));
  b.z = Math.max(-lim, Math.min(lim, b.z));
  return b;
}
