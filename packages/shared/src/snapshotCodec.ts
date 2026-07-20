// Binary wire format for the per-tick GameSnapshot, with delta compression.
//
// JSON.stringify of a snapshot 15×/sec × every connection was the server's
// hottest CPU/bandwidth path. We pack it little-endian, AND only send what
// changed since the last snapshot (QuakeWorld's lesson): names/ids are sent once
// per keyframe and never again; an idle player costs a single mask byte; a
// stationary pickup/barrel array isn't resent. Over WebSocket (reliable, ordered)
// the client's last-received snapshot IS the server's last-sent one, so the
// baseline is implicit — no ack handshake. A periodic keyframe bounds any error
// and lets a fresh stream resync (and readies us for an unreliable transport).
//
// Control messages (match_start/_end, lobby, chat) stay JSON. Binary frames lead
// with a 1-byte tag so the client tells snapshot from voice.

import type { GameSnapshot, MatchPhase, PlayerState, ShotEvent } from "./types.ts";

export const BIN_VOICE = 0; // raw PCM voice frame (server prepends this on relay)
export const BIN_SNAPSHOT = 1; // an encoded GameSnapshot (keyframe or delta)

// Frame flags (2nd byte).
const F_KEYFRAME = 1; // full snapshot (no baseline needed)
const F_PICKUPS = 2; // pickups array is present this frame
const F_BARRELS = 4; // barrels array is present this frame

// Per-player change mask bits (delta frames only).
const M_X = 1, M_Y = 2, M_Z = 4, M_YAW = 8, M_HEALTH = 16, M_KILLS = 32, M_DEATHS = 64, M_ALIVE = 128;

const KEYFRAME_EVERY = 30; // force a full frame at least this often (~2s at 15Hz)

// Index ⇄ phase. Order is the wire contract — only ever append.
const PHASES: MatchPhase[] = ["WAITING", "STARTING", "IN_PROGRESS", "SUDDEN_DEATH", "COMPLETED"];

const SCRATCH = new Uint8Array(4096);
const SCRATCH_DV = new DataView(SCRATCH.buffer);
const UTF8 = new TextEncoder();
const UTF8_DEC = new TextDecoder();

function boolArrEq(a: boolean[], b: boolean[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return false;
  return true;
}

/** Stateful per-stream encoder (one per Match). Holds the last snapshot as the
 *  delta baseline and forces a keyframe periodically. */
export class SnapshotEncoder {
  private baseline: GameSnapshot | null = null;
  private sinceKey = 0;

  encode(s: GameSnapshot): Uint8Array {
    let o = 0;
    const u8 = (v: number) => {
      SCRATCH[o++] = v & 0xff;
    };
    const u32 = (v: number) => {
      SCRATCH_DV.setUint32(o, v >>> 0, true);
      o += 4;
    };
    const f32 = (v: number) => {
      SCRATCH_DV.setFloat32(o, v, true);
      o += 4;
    };
    const str = (v: string) => {
      let b = UTF8.encode(v);
      if (b.length > 255) b = b.subarray(0, 255);
      u8(b.length);
      SCRATCH.set(b, o);
      o += b.length;
    };

    const base = this.baseline;
    const key = base === null || this.sinceKey >= KEYFRAME_EVERY;
    const pickChanged = key || !boolArrEq(s.pickups, base!.pickups);
    const barChanged = key || !boolArrEq(s.barrels, base!.barrels);

    u8(BIN_SNAPSHOT);
    u8((key ? F_KEYFRAME : 0) | (pickChanged ? F_PICKUPS : 0) | (barChanged ? F_BARRELS : 0));
    u32(s.tick);
    u8(Math.max(0, PHASES.indexOf(s.phase)));
    u32(Math.max(0, Math.round(s.timeLeftMs)));

    u8(s.players.length);
    for (let i = 0; i < s.players.length; i++) {
      const p = s.players[i];
      const hp = Math.max(0, Math.min(255, Math.round(p.health)));
      if (key) {
        str(p.id);
        str(p.name);
        f32(p.x);
        f32(p.y);
        f32(p.z);
        f32(p.yaw);
        u8(hp);
        u8(Math.min(255, p.kills));
        u8(Math.min(255, p.deaths));
        u8(p.alive ? 1 : 0);
      } else {
        const b = base!.players[i];
        let mask = 0;
        if (p.x !== b.x) mask |= M_X;
        if (p.y !== b.y) mask |= M_Y;
        if (p.z !== b.z) mask |= M_Z;
        if (p.yaw !== b.yaw) mask |= M_YAW;
        if (hp !== b.health) mask |= M_HEALTH;
        if (p.kills !== b.kills) mask |= M_KILLS;
        if (p.deaths !== b.deaths) mask |= M_DEATHS;
        if (p.alive !== b.alive) mask |= M_ALIVE;
        u8(mask);
        if (mask & M_X) f32(p.x);
        if (mask & M_Y) f32(p.y);
        if (mask & M_Z) f32(p.z);
        if (mask & M_YAW) f32(p.yaw);
        if (mask & M_HEALTH) u8(hp);
        if (mask & M_KILLS) u8(Math.min(255, p.kills));
        if (mask & M_DEATHS) u8(Math.min(255, p.deaths));
        if (mask & M_ALIVE) u8(p.alive ? 1 : 0);
      }
    }

    // shots are transient events — always sent in full
    u8(s.shots.length);
    for (const sh of s.shots) {
      str(sh.shooterId);
      f32(sh.ox);
      f32(sh.oy);
      f32(sh.oz);
      f32(sh.ex);
      f32(sh.ey);
      f32(sh.ez);
      str(sh.hitId ?? "");
      u8(sh.killed ? 1 : 0);
    }

    if (pickChanged) {
      u8(s.pickups.length);
      for (const a of s.pickups) u8(a ? 1 : 0);
    }
    if (barChanged) {
      u8(s.barrels.length);
      for (const b of s.barrels) u8(b ? 1 : 0);
    }

    u8(s.booms.length);
    for (const bm of s.booms) {
      f32(bm.x);
      f32(bm.y);
      f32(bm.z);
    }

    this.baseline = s;
    this.sinceKey = key ? 0 : this.sinceKey + 1;
    return SCRATCH.slice(0, o);
  }
}

/** Stateful per-stream decoder (one per connection). Reconstructs each snapshot
 *  from the keyframe/delta against the previously decoded baseline. */
export class SnapshotDecoder {
  private baseline: GameSnapshot | null = null;

  decode(buf: ArrayBuffer): GameSnapshot {
    const dv = new DataView(buf);
    const bytes = new Uint8Array(buf);
    let o = 1; // skip the BIN_SNAPSHOT tag (the dispatcher already read it)
    const u8 = () => bytes[o++];
    const u32 = () => {
      const v = dv.getUint32(o, true);
      o += 4;
      return v;
    };
    const f32 = () => {
      const v = dv.getFloat32(o, true);
      o += 4;
      return v;
    };
    const str = () => {
      const n = u8();
      const v = UTF8_DEC.decode(bytes.subarray(o, o + n));
      o += n;
      return v;
    };

    const flags = u8();
    const key = (flags & F_KEYFRAME) !== 0;
    const tick = u32();
    const phase = PHASES[u8()] ?? "IN_PROGRESS";
    const timeLeftMs = u32();

    const pc = u8();
    let players: PlayerState[];
    if (key) {
      players = [];
      for (let i = 0; i < pc; i++) {
        const id = str();
        const name = str();
        const x = f32();
        const y = f32();
        const z = f32();
        const yaw = f32();
        const health = u8();
        const kills = u8();
        const deaths = u8();
        const alive = u8() === 1;
        players.push({ id, name, x, y, z, yaw, health, alive, kills, deaths });
      }
    } else {
      const base = this.baseline!;
      players = base.players.map((p) => ({ ...p })); // carry id/name/unchanged fields
      for (let i = 0; i < pc; i++) {
        const mask = u8();
        const p = players[i];
        if (mask & M_X) p.x = f32();
        if (mask & M_Y) p.y = f32();
        if (mask & M_Z) p.z = f32();
        if (mask & M_YAW) p.yaw = f32();
        if (mask & M_HEALTH) p.health = u8();
        if (mask & M_KILLS) p.kills = u8();
        if (mask & M_DEATHS) p.deaths = u8();
        if (mask & M_ALIVE) p.alive = u8() === 1;
      }
    }

    const shots: ShotEvent[] = [];
    for (let n = u8(), i = 0; i < n; i++) {
      const shooterId = str();
      const ox = f32();
      const oy = f32();
      const oz = f32();
      const ex = f32();
      const ey = f32();
      const ez = f32();
      const hit = str();
      const killed = u8() === 1;
      shots.push({ shooterId, ox, oy, oz, ex, ey, ez, hitId: hit === "" ? null : hit, killed });
    }

    const pickups = flags & F_PICKUPS ? readBools(u8, () => u8()) : (this.baseline?.pickups ?? []);
    const barrels = flags & F_BARRELS ? readBools(u8, () => u8()) : (this.baseline?.barrels ?? []);

    const booms: { x: number; y: number; z: number }[] = [];
    for (let n = u8(), i = 0; i < n; i++) booms.push({ x: f32(), y: f32(), z: f32() });

    const snap: GameSnapshot = { matchId: "", tick, phase, timeLeftMs, players, shots, pickups, barrels, booms };
    this.baseline = snap;
    return snap;
  }
}

function readBools(u8: () => number, rd: () => number): boolean[] {
  const out: boolean[] = [];
  for (let n = u8(), i = 0; i < n; i++) out.push(rd() === 1);
  return out;
}
