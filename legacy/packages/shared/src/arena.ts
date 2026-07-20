// Arena primitives. The GEOMETRY itself is no longer a constant — maps are
// seeded-random per match (see mapgen.ts); client and server both generate
// from the seed the server picks, so they can never disagree about the world.

export interface Box {
  // axis-aligned, y0 = bottom, y1 = top
  x0: number;
  x1: number;
  y0: number;
  y1: number;
  z0: number;
  z1: number;
}

export const ARENA_HALF = 30; // 60 x 60 playfield — room for streets, perches and flanks

export interface Spawn {
  x: number;
  z: number;
  yaw: number;
}
