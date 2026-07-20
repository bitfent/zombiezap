// Server-side player: authoritative body + combat state + the input mailbox.
// The client only ever SUGGESTS inputs; everything here is server truth.

import {
  MAX_HEALTH,
  MAX_INPUTS_PER_SECOND,
  MAX_PITCH,
  type Body,
  type PlayerInput,
  type PlayerState,
  type Spawn,
} from "@shotante/shared";

export class ServerPlayer {
  readonly id: string;
  name: string;
  /** Payout wallet for wagered duels (lowercased), null for casual play. */
  wagerAddress: string | null = null;
  /** Chosen stake for wagered play: USD tier (cents) + token. null = casual. */
  wagerTier: number | null = null;
  wagerTokenKind: "usdc" | "eth" | null = null;
  body: Body = { x: 0, y: 0, z: 0, vy: 0, onGround: true };
  yaw = 0;
  pitch = 0;
  health = MAX_HEALTH;
  alive = true;
  kills = 0;
  deaths = 0;
  respawnAt = 0; // ms timestamp when allowed back in
  lastShotAt = 0;
  lastChatAt = 0; // chat flood guard (CHAT_RATE_MS)
  voiceReady = false; // client has granted the mic and may transmit
  // Smoothed round-trip time (ms) from ping/pong — drives lag compensation.
  // Seeded at a typical broadband RTT so the first shots are reasonable.
  rttMs = 80;
  send: (json: string) => void;
  sendBinary: (data: Buffer | ArrayBuffer | Uint8Array) => void;

  // Latest held input wins; the server integrates it once per tick. A flood of
  // packets can't make you faster — it just wastes the cheater's bandwidth.
  private latestInput: PlayerInput | null = null;
  private inputTimes: number[] = [];

  constructor(
    id: string,
    name: string,
    send: (json: string) => void,
    sendBinary: (data: Buffer | ArrayBuffer | Uint8Array) => void = () => {},
  ) {
    this.id = id;
    this.name = name.slice(0, 24) || "anon";
    this.send = send;
    this.sendBinary = sendBinary;
  }

  /** Later identity-bearing messages may carry a better name (e.g. the player
   *  browsed the board first, then joined with a callsign). */
  rename(name: string): void {
    this.name = name.slice(0, 24) || "anon";
  }

  acceptInput(input: PlayerInput, now: number): void {
    // anti-cheat: input flood cap (spec §13 "input rate limiting")
    const cutoff = now - 1000;
    this.inputTimes = this.inputTimes.filter((t) => t > cutoff);
    if (this.inputTimes.length >= MAX_INPUTS_PER_SECOND) return;
    this.inputTimes.push(now);
    // anti-cheat: clamp view angles to sane values
    input.pitch = Math.max(-MAX_PITCH, Math.min(MAX_PITCH, input.pitch));
    if (!Number.isFinite(input.yaw) || !Number.isFinite(input.pitch)) return;
    if (this.latestInput && input.sequence <= this.latestInput.sequence) return;
    this.latestInput = input;
  }

  takeInput(): PlayerInput | null {
    return this.latestInput;
  }

  spawn(at: Spawn): void {
    this.body = { x: at.x, y: 0, z: at.z, vy: 0, onGround: true };
    this.yaw = at.yaw;
    this.pitch = 0;
    this.health = MAX_HEALTH;
    this.alive = true;
    this.latestInput = null;
  }

  toState(): PlayerState {
    return {
      id: this.id,
      name: this.name,
      x: this.body.x,
      y: this.body.y,
      z: this.body.z,
      yaw: this.yaw,
      health: this.health,
      alive: this.alive,
      kills: this.kills,
      deaths: this.deaths,
    };
  }
}
