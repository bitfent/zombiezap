// Binary snapshot decoder for ZombieZap — a faithful TS port of
// crates/zz-core/src/snapshot.rs (the wire truth; change it there first).
// Little-endian, keyframe + change-mask deltas, implicit baseline over the
// reliable ordered WebSocket. Extensions over the ShotAnte ancestor codec:
// quantized i16 positions everywhere, entity-id'd zombies with a
// changed-bitset and i8 position deltas, a loot section present only when
// changed, grenades, and richer player state (ammo/reload/ack).

// Transport tag bytes (crates/zz-core/src/protocol.rs).
export const BIN_INPUT = 0;
export const BIN_SNAPSHOT = 1;
export const BIN_VOICE = 2;

// Header flags.
const F_KEYFRAME = 1;
const F_ZOMBIES = 2;
const F_LOOT = 4;
const F_PAUSED = 8;

// Player delta mask bits (u16).
const PM_X = 1 << 0;
const PM_Y = 1 << 1;
const PM_Z = 1 << 2;
const PM_YAW = 1 << 3;
const PM_PITCH = 1 << 4;
const PM_HEALTH = 1 << 5;
const PM_MAG = 1 << 6;
const PM_RESERVE = 1 << 7;
const PM_GRENADES = 1 << 8;
const PM_KILLS = 1 << 9;
const PM_ALIVE = 1 << 10;
const PM_ACK = 1 << 11;
const PM_RELOAD = 1 << 12;

// Zombie delta mask bits (u8).
const ZM_POS_I8 = 1 << 0;
const ZM_POS_I16 = 1 << 1;
const ZM_YAW = 1 << 2;
const ZM_STATE = 1 << 3;
const ZM_HEALTH = 1 << 4;

/** Position quantization: 1/128 m (±256 m range, ~8 mm precision). */
export const POS_SCALE = 128;

export function dequantPos(v: number): number {
  return v / POS_SCALE;
}

export function dequantYaw16(q: number): number {
  return (q / 65536) * Math.PI * 2;
}

export function dequantYaw8(q: number): number {
  return (q / 256) * Math.PI * 2;
}

export function dequantPitch(q: number): number {
  return (q / 127) * (Math.PI / 2);
}

// ── wire state (quantized, exactly as sent) ────────────────────────────────

export interface WirePlayer {
  slot: number;
  pos: [number, number, number]; // i16 quantized
  yaw: number; // u16 quantized
  pitch: number; // i8 quantized
  health: number;
  ammoMag: number;
  ammoReserve: number;
  grenades: number;
  kills: number;
  alive: boolean;
  lastAckedSeq: number;
  reloadTicksLeft: number;
}

export interface WireZombie {
  id: number;
  kind: number;
  state: number;
  pos: [number, number, number];
  yaw: number; // u8 quantized
  health: number;
}

export interface WireLoot {
  id: number;
  kind: number;
  pos: [number, number, number];
}

export interface WireGrenade {
  id: number;
  pos: [number, number, number];
}

export interface WireShot {
  slot: number;
  end: [number, number, number];
  /** 0 = wall/miss, 1 = zombie hit, 2 = zombie killed, 3 = headshot kill. */
  hitKind: number;
}

export interface WireBoom {
  pos: [number, number, number];
}

export interface ZzSnapshot {
  tick: number;
  gameTimeMs: number;
  difficulty: number;
  paused: boolean;
  players: WirePlayer[];
  zombies: WireZombie[];
  loot: WireLoot[];
  grenades: WireGrenade[];
  shots: WireShot[];
  booms: WireBoom[];
}

export class CodecError extends Error {}

class Reader {
  private dv: DataView;
  private bytes: Uint8Array;
  at = 0;

  constructor(buf: ArrayBuffer) {
    this.dv = new DataView(buf);
    this.bytes = new Uint8Array(buf);
  }

  u8(): number {
    if (this.at >= this.bytes.length) throw new CodecError("truncated");
    return this.bytes[this.at++];
  }
  i8(): number {
    const v = this.u8();
    return v > 127 ? v - 256 : v;
  }
  u16(): number {
    const v = this.dv.getUint16(this.at, true);
    this.at += 2;
    return v;
  }
  i16(): number {
    const v = this.dv.getInt16(this.at, true);
    this.at += 2;
    return v;
  }
  u32(): number {
    const v = this.dv.getUint32(this.at, true);
    this.at += 4;
    return v;
  }
  pos(): [number, number, number] {
    return [this.i16(), this.i16(), this.i16()];
  }
  take(n: number): Uint8Array {
    if (this.at + n > this.bytes.length) throw new CodecError("truncated");
    const s = this.bytes.subarray(this.at, this.at + n);
    this.at += n;
    return s;
  }
}

/** Stateful per-connection decoder — one per WebSocket, reset on reconnect. */
export class SnapshotDecoder {
  private baseline: ZzSnapshot | null = null;

  decode(buf: ArrayBuffer): ZzSnapshot {
    const r = new Reader(buf);
    if (r.u8() !== BIN_SNAPSHOT) throw new CodecError("bad tag");
    const flags = r.u8();
    const key = (flags & F_KEYFRAME) !== 0;
    const tick = r.u32();
    const gameTimeMs = r.u32();
    const difficulty = r.u8();

    if (!key) {
      if (this.baseline === null) throw new CodecError("no baseline");
      if (tick <= this.baseline.tick) throw new CodecError("desync");
    }

    // players
    const pc = r.u8();
    let players: WirePlayer[];
    if (key) {
      players = [];
      for (let i = 0; i < pc; i++) players.push(readPlayerFull(r));
    } else {
      const base = this.baseline!;
      if (pc !== base.players.length) throw new CodecError("desync");
      players = base.players.map((p) => ({ ...p, pos: [...p.pos] as [number, number, number] }));
      for (const p of players) readPlayerDelta(r, p);
    }

    // zombies
    let zombies: WireZombie[];
    if ((flags & F_ZOMBIES) !== 0) {
      if (key) {
        const n = r.u16();
        zombies = [];
        for (let i = 0; i < n; i++) zombies.push(readZombieFull(r));
      } else {
        zombies = readZombieDelta(r, this.baseline!.zombies);
      }
    } else {
      zombies = this.baseline?.zombies ?? [];
    }

    // loot
    let loot: WireLoot[];
    if ((flags & F_LOOT) !== 0) {
      const n = r.u16();
      loot = [];
      for (let i = 0; i < n; i++) loot.push({ id: r.u16(), kind: r.u8(), pos: r.pos() });
    } else {
      loot = this.baseline?.loot ?? [];
    }

    // grenades in flight
    const grenades: WireGrenade[] = [];
    for (let n = r.u8(), i = 0; i < n; i++) grenades.push({ id: r.u8(), pos: r.pos() });

    // transient events
    const shots: WireShot[] = [];
    for (let n = r.u8(), i = 0; i < n; i++)
      shots.push({ slot: r.u8(), end: r.pos(), hitKind: r.u8() });
    const booms: WireBoom[] = [];
    for (let n = r.u8(), i = 0; i < n; i++) booms.push({ pos: r.pos() });

    const snap: ZzSnapshot = {
      tick,
      gameTimeMs,
      difficulty,
      paused: (flags & F_PAUSED) !== 0,
      players,
      zombies,
      loot,
      grenades,
      shots,
      booms,
    };
    this.baseline = snap;
    return snap;
  }
}

function readPlayerFull(r: Reader): WirePlayer {
  return {
    slot: r.u8(),
    pos: r.pos(),
    yaw: r.u16(),
    pitch: r.i8(),
    health: r.u8(),
    ammoMag: r.u8(),
    ammoReserve: r.u8(),
    grenades: r.u8(),
    kills: r.u16(),
    alive: r.u8() !== 0,
    lastAckedSeq: r.u32(),
    reloadTicksLeft: r.u8(),
  };
}

function readPlayerDelta(r: Reader, p: WirePlayer): void {
  const mask = r.u16();
  if (mask & PM_X) p.pos[0] = r.i16();
  if (mask & PM_Y) p.pos[1] = r.i16();
  if (mask & PM_Z) p.pos[2] = r.i16();
  if (mask & PM_YAW) p.yaw = r.u16();
  if (mask & PM_PITCH) p.pitch = r.i8();
  if (mask & PM_HEALTH) p.health = r.u8();
  if (mask & PM_MAG) p.ammoMag = r.u8();
  if (mask & PM_RESERVE) p.ammoReserve = r.u8();
  if (mask & PM_GRENADES) p.grenades = r.u8();
  if (mask & PM_KILLS) p.kills = r.u16();
  if (mask & PM_ALIVE) p.alive = r.u8() !== 0;
  if (mask & PM_ACK) p.lastAckedSeq = r.u32();
  if (mask & PM_RELOAD) p.reloadTicksLeft = r.u8();
}

function readZombieFull(r: Reader): WireZombie {
  return {
    id: r.u16(),
    kind: r.u8(),
    state: r.u8(),
    pos: r.pos(),
    yaw: r.u8(),
    health: r.u8(),
  };
}

function readZombieDelta(r: Reader, base: WireZombie[]): WireZombie[] {
  const removed = new Set<number>();
  for (let n = r.u16(), i = 0; i < n; i++) removed.add(r.u16());
  const added: WireZombie[] = [];
  for (let n = r.u16(), i = 0; i < n; i++) added.push(readZombieFull(r));

  // Survivors in BASELINE order (the encoder's bitset contract), added last.
  const survivors: WireZombie[] = base
    .filter((z) => !removed.has(z.id))
    .map((z) => ({ ...z, pos: [...z.pos] as [number, number, number] }));

  const bits = r.take(Math.ceil(survivors.length / 8));
  for (let i = 0; i < survivors.length; i++) {
    if ((bits[i >> 3] & (1 << (i % 8))) === 0) continue;
    const z = survivors[i];
    const mask = r.u8();
    if (mask & ZM_POS_I8) {
      z.pos[0] += r.i8();
      z.pos[1] += r.i8();
      z.pos[2] += r.i8();
    } else if (mask & ZM_POS_I16) {
      z.pos = r.pos();
    }
    if (mask & ZM_YAW) z.yaw = r.u8();
    if (mask & ZM_STATE) z.state = r.u8();
    if (mask & ZM_HEALTH) z.health = r.u8();
  }
  survivors.push(...added);
  return survivors;
}
