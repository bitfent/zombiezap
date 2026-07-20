// Shared protocol types (spec §9). The client and server import THESE — there
// is no second definition to drift.

export interface PlayerInput {
  sequence: number;
  forward: boolean;
  backward: boolean;
  left: boolean;
  right: boolean;
  jump: boolean;
  shoot: boolean;
  yaw: number;
  pitch: number;
}

export interface PlayerState {
  id: string;
  name: string;
  x: number;
  y: number;
  z: number;
  yaw: number;
  health: number;
  alive: boolean;
  kills: number;
  deaths: number;
}

export interface ShotEvent {
  shooterId: string;
  // origin + normalized direction + endpoint (where the tracer stops)
  ox: number;
  oy: number;
  oz: number;
  ex: number;
  ey: number;
  ez: number;
  hitId: string | null;
  killed: boolean;
}

export type MatchPhase = "WAITING" | "STARTING" | "IN_PROGRESS" | "SUDDEN_DEATH" | "COMPLETED";

// Full lifecycle from spec §10 — the escrow-aware states are wired in Phase 4;
// kept here so the settlement layer and the game server share one vocabulary.
export type MatchLifecycle =
  | "CREATED"
  | "WAITING_FOR_PLAYERS"
  | "ESCROW_PENDING"
  | "ESCROW_LOCKED"
  | "STARTING"
  | "IN_PROGRESS"
  | "COMPLETED"
  | "RESULT_SIGNED"
  | "SETTLEMENT_PENDING"
  | "SETTLED"
  | "CANCELLED"
  | "REFUNDED"
  | "DISPUTED"
  | "VOIDED";

export interface GameSnapshot {
  matchId: string;
  tick: number;
  phase: MatchPhase;
  timeLeftMs: number;
  players: PlayerState[];
  shots: ShotEvent[];
  pickups: boolean[]; // active flag per arena pickup (same order as Arena.pickups)
  barrels: boolean[]; // intact flag per arena barrel (same order as Arena.barrels)
  booms: { x: number; y: number; z: number }[]; // explosions this snapshot window
}

export interface MatchResult {
  matchId: string;
  players: { id: string; name: string; kills: number; deaths: number }[];
  winnerId: string | null; // null = void
  reason: "kills" | "time" | "sudden_death" | "forfeit" | "void";
  startedAt: number;
  endedAt: number;
  // sha256 over the canonical result JSON — the value the escrow contract's
  // authorized signer signs in Phase 4 (spec §12).
  resultHash: string;
}
