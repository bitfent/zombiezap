// Authoritative hitscan (spec §13): the ray starts at the shooter's SERVER
// position, walls occlude, and the nearest player sphere within range gets the
// damage. The client's opinion about what it hit is never consulted.

import {
  PLAYER_EYE,
  PLAYER_HITBOX,
  SHOT_RANGE,
  dirFromAngles,
  nearestWallHit,
  raySphere,
  type Box,
  type ShotEvent,
} from "@shotante/shared";
import type { ServerPlayer } from "./Player.ts";

/** A target's hittable position — usually its server-current body, but lag
 *  compensation feeds REWOUND coordinates (where the target was on the shooter's
 *  screen) so a high-ping shot that looked dead-on still registers. */
export interface HitTarget {
  player: ServerPlayer;
  x: number;
  y: number;
  z: number;
}

/** wallIndex: index (into `walls`) of the box the shot stopped on, or null if
 *  it hit a player / flew out to max range — lets Match detonate barrels. */
export function fireHitscan(
  shooter: ServerPlayer,
  targets: HitTarget[],
  walls: Box[],
): { shot: ShotEvent; wallIndex: number | null } {
  const o = { x: shooter.body.x, y: shooter.body.y + PLAYER_EYE, z: shooter.body.z };
  const d = dirFromAngles(shooter.yaw, shooter.pitch);

  const wallHit = nearestWallHit(o, d, walls);
  let bestT = wallHit !== null ? Math.min(wallHit.t, SHOT_RANGE) : SHOT_RANGE;
  let hit: ServerPlayer | null = null;

  for (const tg of targets) {
    if (!tg.player.alive || tg.player.id === shooter.id) continue;
    // head/torso/legs spheres — nearest hit on any of them counts. Position is
    // the (possibly rewound) target location, not necessarily the current body.
    for (const hb of PLAYER_HITBOX) {
      const t = raySphere(o, d, { x: tg.x, y: tg.y + hb.y, z: tg.z }, hb.r);
      if (t !== null && t < bestT) {
        bestT = t;
        hit = tg.player;
      }
    }
  }

  const stoppedOnWall = hit === null && wallHit !== null && wallHit.t <= SHOT_RANGE;
  return {
    shot: {
      shooterId: shooter.id,
      ox: o.x,
      oy: o.y,
      oz: o.z,
      ex: o.x + d.x * bestT,
      ey: o.y + d.y * bestT,
      ez: o.z + d.z * bestT,
      hitId: hit ? hit.id : null,
      killed: false, // Match fills this in after applying damage
    },
    wallIndex: stoppedOnWall ? wallHit.index : null,
  };
}
