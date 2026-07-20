// Round-trip test for the delta-compressed snapshot codec. The codec only
// type-imports (no runtime deps), so this runs standalone:
//   node --experimental-strip-types scripts/test-snapshot-codec.ts
import { SnapshotEncoder, SnapshotDecoder, BIN_SNAPSHOT } from "../packages/shared/src/snapshotCodec.ts";
import type { GameSnapshot } from "../packages/shared/src/types.ts";

const F = (a: number, b: number) => Math.abs(a - b) < 1e-3; // float32 tolerance
const fail = (m: string) => {
  console.error("FAIL:", m);
  process.exit(1);
};

const frame1: GameSnapshot = {
  matchId: "m-abc",
  tick: 1234,
  phase: "IN_PROGRESS",
  timeLeftMs: 59321,
  players: [
    { id: "0a1b2c3d", name: "sharpshooter", x: 12.5, y: 0, z: -7.25, yaw: 1.5708, health: 100, alive: true, kills: 0, deaths: 0 },
    { id: "deadbeef", name: "sitting_duck", x: -3.1, y: 1.4, z: 18.9, yaw: -2.3, health: 100, alive: true, kills: 0, deaths: 0 },
  ],
  shots: [{ shooterId: "0a1b2c3d", ox: 1, oy: 1.5, oz: 2, ex: 10, ey: 1.6, ez: 12, hitId: "deadbeef", killed: false }],
  pickups: [true, false, true],
  barrels: [true, true, false, true],
  booms: [],
};

// frame2: player 0 moved + took a kill, player 1 unchanged, a barrel detonated, a boom
const frame2: GameSnapshot = {
  ...frame1,
  tick: 1236,
  timeLeftMs: 59255,
  players: [
    { ...frame1.players[0], x: 13.0, z: -6.9, yaw: 1.6, kills: 1 },
    { ...frame1.players[1], health: 75, alive: false, deaths: 1 },
  ],
  shots: [],
  barrels: [true, true, false, false],
  booms: [{ x: 3, y: 0.5, z: 4 }],
};

const enc = new SnapshotEncoder();
const dec = new SnapshotDecoder();
const copy = (u: Uint8Array) => u.buffer.slice(u.byteOffset, u.byteOffset + u.byteLength);

// ── frame 1: keyframe ──
const b1 = enc.encode(frame1);
if (b1[0] !== BIN_SNAPSHOT || (b1[1] & 1) !== 1) fail("frame1 should be a keyframe");
const out1 = dec.decode(copy(b1));
if (out1.players.length !== 2) fail("f1 players");
for (let i = 0; i < 2; i++) {
  const a = frame1.players[i], b = out1.players[i];
  if (a.id !== b.id || a.name !== b.name || a.health !== b.health || a.alive !== b.alive || a.kills !== b.kills) fail(`f1 player ${i}`);
  if (!F(a.x, b.x) || !F(a.y, b.y) || !F(a.z, b.z) || !F(a.yaw, b.yaw)) fail(`f1 player ${i} pos`);
}
if (out1.shots.length !== 1 || out1.shots[0].hitId !== "deadbeef") fail("f1 shots");
if (JSON.stringify(out1.pickups) !== JSON.stringify(frame1.pickups)) fail("f1 pickups");
if (JSON.stringify(out1.barrels) !== JSON.stringify(frame1.barrels)) fail("f1 barrels");

// ── frame 2: delta ──
const b2 = enc.encode(frame2);
if ((b2[1] & 1) !== 0) fail("frame2 should be a delta");
const out2 = dec.decode(copy(b2));
// player 0 moved + got a kill
if (!F(out2.players[0].x, 13.0) || !F(out2.players[0].z, -6.9) || !F(out2.players[0].yaw, 1.6)) fail("f2 p0 pos");
if (out2.players[0].kills !== 1) fail("f2 p0 kills");
// player 1: name/id carried from baseline, health/alive/deaths updated, position unchanged
if (out2.players[1].name !== "sitting_duck" || out2.players[1].id !== "deadbeef") fail("f2 p1 identity carried");
if (out2.players[1].health !== 75 || out2.players[1].alive !== false || out2.players[1].deaths !== 1) fail("f2 p1 status");
if (!F(out2.players[1].x, -3.1) || !F(out2.players[1].z, 18.9)) fail("f2 p1 pos carried");
// barrels changed (sent), pickups unchanged (carried from baseline)
if (JSON.stringify(out2.barrels) !== JSON.stringify(frame2.barrels)) fail("f2 barrels");
if (JSON.stringify(out2.pickups) !== JSON.stringify(frame1.pickups)) fail("f2 pickups carried");
if (out2.booms.length !== 1 || !F(out2.booms[0].x, 3)) fail("f2 booms");
if (out2.shots.length !== 0) fail("f2 shots empty");

const jsonBytes = Buffer.byteLength(JSON.stringify(frame2));
console.log(`PASS: keyframe + delta round-trip OK.`);
console.log(`  keyframe=${b1.byteLength}B, delta=${b2.byteLength}B vs json=${jsonBytes}B (delta ${Math.round((1 - b2.byteLength / jsonBytes) * 100)}% smaller than JSON)`);
