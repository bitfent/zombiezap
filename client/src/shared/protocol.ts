// ZombieZap control protocol (JSON over WebSocket) + the binary input frame.
// Mirrors crates/zz-core/src/protocol.rs exactly — that file is the wire
// truth; change it there first, then here. Serde: enum variants are tagged
// via `type` in snake_case; struct fields keep their Rust snake_case names.

export type EnvKind = "urban" | "mountain_town" | "desert_town" | "sea_town" | "rome_eur";

// ── client → server ────────────────────────────────────────────────────────

export type ClientMessage =
  | { type: "hello"; name: string }
  | { type: "create_lobby"; env: EnvKind }
  | { type: "join_lobby"; code: string }
  | { type: "set_env"; env: EnvKind }
  | { type: "start_game" }
  | { type: "leave_lobby" }
  | { type: "pause" }
  | { type: "resume" }
  | { type: "pong"; t: number };

// ── server → client ────────────────────────────────────────────────────────

export interface LobbyPlayer {
  id: string;
  name: string;
}

export interface RosterPlayer {
  slot: number;
  id: string;
  name: string;
}

export interface PlayerStats {
  slot: number;
  name: string;
  kills: number;
  damage_dealt: number;
  shots_fired: number;
  hits: number;
  grenades_thrown: number;
  time_alive_ms: number;
}

export interface MatchStats {
  duration_ms: number;
  zombies_killed: number;
  peak_zombies: number;
  difficulty_reached: number;
  waves_cleared: number;
  players: PlayerStats[];
}

export type ServerMessage =
  | { type: "welcome"; player_id: string; protocol: number }
  | {
      type: "lobby_state";
      code: string;
      host_id: string;
      players: LobbyPlayer[];
      env: EnvKind;
      invite_url: string | null;
    }
  | {
      type: "game_start";
      map_seed: string;
      env: EnvKind;
      your_slot: number;
      players: RosterPlayer[];
    }
  | { type: "paused"; by: string }
  | { type: "resumed" }
  | { type: "player_left"; id: string }
  | { type: "ping"; t: number }
  | { type: "match_end"; stats: MatchStats }
  | { type: "wave_start"; wave: number }
  | { type: "wave_clear"; wave: number; bonus_ammo: number }
  | { type: "supply_drop"; id: number; x: number; z: number }
  | {
      type: "cover_smashed";
      x0: number;
      x1: number;
      y0: number;
      y1: number;
      z0: number;
      z1: number;
    }
  | { type: "error"; message: string };

// ── binary input frame ─────────────────────────────────────────────────────
//
// 15 bytes: [0]=BIN_INPUT tag, [1..5]=seq u32 LE, [5]=buttons bitfield
// (0 fwd, 1 back, 2 left, 3 right, 4 jump, 5 fire, 6 grenade, 7 interact),
// [6]=flags2 (bit0 melee, bit1 reload), [7..11]=yaw f32 LE, [11..15]=pitch f32 LE.

export const INPUT_FRAME_LEN = 15;

import type { PlayerInput } from "./types.ts";

/** zz-only buttons the ShotAnte input path doesn't produce yet — optional so
 *  carried-forward call sites compile; encode treats absent as false. */
export interface ZzInputExtras {
  grenade?: boolean;
  interact?: boolean;
  melee?: boolean;
  reload?: boolean;
}

export function encodeInput(input: PlayerInput & ZzInputExtras): Uint8Array {
  const frame = new Uint8Array(INPUT_FRAME_LEN);
  const dv = new DataView(frame.buffer);
  frame[0] = 0; // BIN_INPUT
  dv.setUint32(1, input.sequence >>> 0, true);
  let b = 0;
  if (input.forward) b |= 1 << 0;
  if (input.backward) b |= 1 << 1;
  if (input.left) b |= 1 << 2;
  if (input.right) b |= 1 << 3;
  if (input.jump) b |= 1 << 4;
  if (input.shoot) b |= 1 << 5;
  if (input.grenade) b |= 1 << 6;
  if (input.interact) b |= 1 << 7;
  frame[5] = b;
  let f2 = 0;
  if (input.melee) f2 |= 1 << 0;
  if (input.reload) f2 |= 1 << 1;
  frame[6] = f2;
  dv.setFloat32(7, input.yaw, true);
  dv.setFloat32(11, input.pitch, true);
  return frame;
}
