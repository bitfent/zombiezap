// One 1v1 duel: authoritative simulation at a fixed tick rate, snapshots out,
// result hashing at the end. The lifecycle here is the GAME slice of spec §10
// (WAITING -> STARTING -> IN_PROGRESS [-> SUDDEN_DEATH] -> COMPLETED); the
// escrow states wrap around it in Phase 4.

import {
  BARREL_DMG_MAX,
  BARREL_DMG_MIN,
  BARREL_RADIUS_DMG,
  CHAT_PROXIMITY_RADIUS,
  COUNTDOWN_MS,
  DAMAGE_PER_HIT,
  FIRE_COOLDOWN_MS,
  INTERP_DELAY_MS,
  KILLS_TO_WIN,
  MATCH_DURATION_MS,
  MAX_REWIND_MS,
  PICKUP_HEAL,
  PICKUP_RADIUS,
  PICKUP_RESPAWN_MS,
  RECONNECT_GRACE_MS,
  RESPAWN_MS,
  SNAPSHOT_EVERY_TICKS,
  SUDDEN_DEATH_MAX_MS,
  TICK_DT,
  TICK_RATE,
  BIN_VOICE,
  SnapshotEncoder,
  generateArena,
  stepBody,
  type Arena,
  type Box,
  type GameSnapshot,
  type MatchPhase,
  type MatchResult,
  type ServerMessage,
  type ShotEvent,
} from "@shotante/shared";
import { fireHitscan, type HitTarget } from "./HitScan.ts";
import { writeReplay, type ReplayFrame } from "../replay.ts";
import type { ServerPlayer } from "./Player.ts";
import { finalizeResult } from "../settlement/signResult.ts";
import { canRelaySettlement, relaySettlement, signSettlement } from "../settlement/escrow.ts";
import { recordMatch } from "../db.ts";
import type { WagerTerms } from "@shotante/shared";

let matchCounter = 0;

export class Match {
  readonly id: string;
  phase: MatchPhase = "STARTING";
  private readonly players: ServerPlayer[];
  private tick = 0;
  private shots: ShotEvent[] = [];
  private startedAt = 0;
  private endsAt = 0;
  private timer: ReturnType<typeof setInterval> | null = null;
  private onEnd: (m: Match) => void;
  private readonly wager: WagerTerms | null;
  private readonly arena: Arena;
  private pickups: { x: number; z: number; active: boolean; respawnAt: number }[];
  // Explosive barrels: liveWalls is arena.walls minus detonated barrels — the
  // array movement and hitscan actually use, so a destroyed barrel stops
  // blocking and occluding the same tick on both sides.
  private liveWalls: Box[];
  private barrelIntact: boolean[];
  private barrelByBox = new Map<Box, number>();
  private booms: { x: number; y: number; z: number }[] = [];
  // Lag compensation: a ~1s ring buffer of every player's position per tick, so a
  // shot can be tested against where the target WAS on the shooter's screen.
  private history: { t: number; pos: { id: string; x: number; y: number; z: number }[] }[] = [];
  // Delta-compresses the snapshot stream (keyframe + per-field deltas).
  private encoder = new SnapshotEncoder();
  // Replay recording: wagered duels always (disputes/audit), free duels only when
  // RECORD_REPLAYS=1 — keeps the 5k-duel RAM budget intact by default.
  private recording = false;
  private readonly createdAt = Date.now();
  private replayFrames: ReplayFrame[] = [];
  // Reconnect grace: when a player drops mid-match the duel PAUSES (sim + clock
  // frozen) and they get RECONNECT_GRACE_MS to re-enter. One grace per player.
  private paused = false;
  private pauseStart = 0;
  private graceTimer: ReturnType<typeof setTimeout> | null = null;
  private graceUsed = new Set<string>();

  constructor(a: ServerPlayer, b: ServerPlayer, wager: WagerTerms | null, onEnd: (m: Match) => void) {
    this.id = `m${Date.now().toString(36)}-${(matchCounter++).toString(36)}`;
    this.players = [a, b];
    this.wager = wager;
    this.onEnd = onEnd;
    this.recording = wager !== null || process.env.RECORD_REPLAYS === "1";
    // Seeded-random map per match; MAP_SEED pins it (tests/ops). The match id
    // is unique per match, so it doubles as the seed.
    this.arena = generateArena(process.env.MAP_SEED ?? this.id);
    this.pickups = this.arena.pickups.map((p) => ({ ...p, active: true, respawnAt: 0 }));
    this.liveWalls = [...this.arena.walls];
    this.barrelIntact = this.arena.barrels.map(() => true);
    this.arena.barrels.forEach((b, i) => this.barrelByBox.set(this.arena.walls[b.wallIndex], i));
    a.kills = a.deaths = b.kills = b.deaths = 0;
    a.spawn(this.arena.spawns[0]);
    b.spawn(this.arena.spawns[1]);

    this.broadcast({
      type: "match_start",
      matchId: this.id,
      countdownMs: COUNTDOWN_MS,
      players: this.players.map((p) => ({ id: p.id, name: p.name })),
      wagered: this.wager !== null,
      mapSeed: this.arena.seed,
    });

    setTimeout(() => {
      this.phase = "IN_PROGRESS";
      this.startedAt = Date.now();
      this.endsAt = this.startedAt + MATCH_DURATION_MS;
    }, COUNTDOWN_MS);

    this.timer = setInterval(() => this.step(), 1000 / TICK_RATE);
  }

  get playerIds(): string[] {
    return this.players.map((p) => p.id);
  }

  /** Proximity VOICE: forward a raw PCM audio frame to the opponent, but only
   *  when they are CLOSE (server-authoritative positions). Out of range =
   *  silently dropped — that IS the mechanic: close the distance to be heard.
   *  Ephemeral; never stored. The client also gates sending, this is the
   *  authoritative backstop. */
  relayVoice(senderId: string, frame: Buffer | ArrayBuffer | Uint8Array): void {
    const sender = this.players.find((p) => p.id === senderId);
    const other = this.players.find((p) => p.id !== senderId);
    if (!sender || !other) return;
    const dx = sender.body.x - other.body.x;
    const dz = sender.body.z - other.body.z;
    if (dx * dx + dz * dz > CHAT_PROXIMITY_RADIUS * CHAT_PROXIMITY_RADIUS) return;
    // prepend the BIN_VOICE tag so the client tells voice from snapshot frames
    const src = frame instanceof Uint8Array ? frame : new Uint8Array(frame);
    const tagged = new Uint8Array(src.length + 1);
    tagged[0] = BIN_VOICE;
    tagged.set(src, 1);
    other.sendBinary(tagged);
  }

  handleDisconnect(playerId: string): void {
    if (this.phase === "COMPLETED") return;
    const other = this.players.find((p) => p.id !== playerId) ?? null;
    const live = this.phase === "IN_PROGRESS" || this.phase === "SUDDEN_DEATH";
    // First drop of a LIVE duel: PAUSE and give them one grace window to re-enter,
    // rather than forfeiting on a flaky connection. Otherwise (no opponent, second
    // drop, or pre-game) forfeit now.
    if (other && live && !this.paused && !this.graceUsed.has(playerId)) {
      this.graceUsed.add(playerId);
      this.paused = true;
      this.pauseStart = Date.now();
      other.send(JSON.stringify({ type: "opponent_dropped", graceMs: RECONNECT_GRACE_MS }));
      this.graceTimer = setTimeout(() => {
        this.graceTimer = null;
        this.finish(other.id, "forfeit"); // never came back
      }, RECONNECT_GRACE_MS);
      return;
    }
    this.finish(other ? other.id : null, other ? "forfeit" : "void");
  }

  /** A dropped player re-entered within the grace window: unpause, un-freeze the
   *  clock (shift the deadlines by however long we were paused), and tell both
   *  sides. The queue has already rebound the player's send to the new socket. */
  reconnect(playerId: string): boolean {
    if (this.phase === "COMPLETED") return false;
    if (this.graceTimer) {
      clearTimeout(this.graceTimer);
      this.graceTimer = null;
    }
    if (this.paused) {
      this.paused = false;
      this.endsAt += Date.now() - this.pauseStart; // the clock stood still while we waited
    }
    const back = this.players.find((p) => p.id === playerId);
    const other = this.players.find((p) => p.id !== playerId);
    back?.send(JSON.stringify({ type: "match_resumed" }));
    other?.send(JSON.stringify({ type: "opponent_reconnected" }));
    return true;
  }

  private step(): void {
    if (this.phase === "COMPLETED") return;
    if (this.paused) return; // a player dropped — frozen until they re-enter or forfeit
    const now = Date.now();

    if (this.phase === "IN_PROGRESS" || this.phase === "SUDDEN_DEATH") {
      // 1) respawns
      for (const p of this.players) {
        if (!p.alive && now >= p.respawnAt) {
          // spawn at the point farthest from the opponent
          const other = this.players.find((o) => o.id !== p.id)!;
          const spawns = this.arena.spawns;
          const spawn = spawns.reduce((best, s) => {
            const d = (s.x - other.body.x) ** 2 + (s.z - other.body.z) ** 2;
            const bd = (best.x - other.body.x) ** 2 + (best.z - other.body.z) ** 2;
            return d > bd ? s : best;
          }, spawns[0]);
          p.spawn(spawn);
        }
      }

      // 1b) health pickups: respawn timers + grabs (server-authoritative)
      for (const pk of this.pickups) {
        if (!pk.active) {
          if (now >= pk.respawnAt) pk.active = true;
          continue;
        }
        for (const p of this.players) {
          if (!p.alive || p.health >= 100) continue;
          const dsq = (p.body.x - pk.x) ** 2 + (p.body.z - pk.z) ** 2;
          if (dsq < PICKUP_RADIUS * PICKUP_RADIUS) {
            p.health = Math.min(100, p.health + PICKUP_HEAL);
            pk.active = false;
            pk.respawnAt = now + PICKUP_RESPAWN_MS;
            break;
          }
        }
      }

      // 2) movement + combat from the latest held input
      for (const p of this.players) {
        const input = p.takeInput();
        if (!p.alive || !input) continue;
        p.yaw = input.yaw;
        p.pitch = input.pitch;
        stepBody(p.body, input, TICK_DT, this.liveWalls);

        if (input.shoot && now - p.lastShotAt >= FIRE_COOLDOWN_MS) {
          p.lastShotAt = now; // anti-cheat: cooldown enforced server-side
          // Hits test the target's CURRENT position. (Lag compensation — rewinding
          // to the shooter's view-time — is disabled: the clock-offset estimate was
          // an approximation that degraded at real ping and hurt aim. Re-enable only
          // with proper command timestamps. rewoundTargets/history kept dormant.)
          const { shot, wallIndex } = fireHitscan(p, this.currentTargets(p), this.liveWalls);
          if (shot.hitId) {
            const victim = this.players.find((v) => v.id === shot.hitId)!;
            victim.health -= DAMAGE_PER_HIT;
            if (victim.health <= 0) {
              victim.health = 0;
              victim.alive = false;
              victim.deaths += 1;
              victim.respawnAt = now + RESPAWN_MS;
              p.kills += 1;
              shot.killed = true;
            }
          } else if (wallIndex !== null) {
            // shot stopped on a wall — was it an intact barrel?
            const barrelIdx = this.barrelByBox.get(this.liveWalls[wallIndex]);
            if (barrelIdx !== undefined) this.explode(barrelIdx, now);
          }
          this.shots.push(shot);
        }
      }

      // 3) end conditions
      const leader = [...this.players].sort((x, y) => y.kills - x.kills);
      if (this.phase === "SUDDEN_DEATH") {
        if (leader[0].kills > leader[1].kills) {
          this.finish(leader[0].id, "sudden_death");
          return;
        }
        // liveness: nobody may stall a wagered match forever — still tied
        // after the cap is a draw; escrowed antes return via refund()
        if (now >= this.endsAt + SUDDEN_DEATH_MAX_MS) {
          this.finish(null, "void");
          return;
        }
      }
      if (leader[0].kills >= KILLS_TO_WIN) {
        this.finish(leader[0].id, "kills");
        return;
      }
      if (this.phase === "IN_PROGRESS" && now >= this.endsAt) {
        if (leader[0].kills === leader[1].kills) {
          this.phase = "SUDDEN_DEATH"; // next kill wins, no clock
        } else {
          this.finish(leader[0].id, "time");
          return;
        }
      }
    }

    // 4) snapshots — the hot path: packed binary (see snapshotCodec), not JSON
    this.tick += 1;
    if (this.tick % SNAPSHOT_EVERY_TICKS === 0) {
      const bin = this.encoder.encode(this.snapshot(now));
      for (const p of this.players) p.sendBinary(bin);
      if (this.recording) this.replayFrames.push({ t: now - this.createdAt, bytes: bin });
      this.shots = [];
      this.booms = [];
    }
  }

  /** Detonate barrel `idx`: radius damage with linear falloff to BOTH players
   *  (a self-kill counts as your death and credits the opponent — the 5-kill
   *  economy stays clean), then chain into other intact barrels in range. */
  private explode(idx: number, now: number): void {
    if (!this.barrelIntact[idx]) return;
    this.barrelIntact[idx] = false;
    const b = this.arena.barrels[idx];
    const box = this.arena.walls[b.wallIndex];
    const center = { x: b.x, y: (box.y0 + box.y1) / 2, z: b.z };
    this.booms.push(center);
    this.liveWalls = this.liveWalls.filter((w) => w !== box);
    this.barrelByBox.delete(box);

    for (const v of this.players) {
      if (!v.alive) continue;
      // 3D distance to the player's torso — a bridge deck above stays safe
      const d = Math.hypot(v.body.x - center.x, v.body.y + 0.9 - center.y, v.body.z - center.z);
      if (d > BARREL_RADIUS_DMG) continue;
      const dmg = Math.round(BARREL_DMG_MAX - (BARREL_DMG_MAX - BARREL_DMG_MIN) * (d / BARREL_RADIUS_DMG));
      v.health -= dmg;
      if (v.health <= 0) {
        v.health = 0;
        v.alive = false;
        v.deaths += 1;
        v.respawnAt = now + RESPAWN_MS;
        const other = this.players.find((o) => o.id !== v.id);
        if (other) other.kills += 1;
      }
    }

    // chain reaction: nearby intact barrels go up too (same tick)
    this.arena.barrels.forEach((nb, j) => {
      if (!this.barrelIntact[j]) return;
      const nbox = this.arena.walls[nb.wallIndex];
      const ny = (nbox.y0 + nbox.y1) / 2;
      if (Math.hypot(nb.x - center.x, ny - center.y, nb.z - center.z) <= BARREL_RADIUS_DMG) {
        this.explode(j, now);
      }
    });
  }

  /** Targets at their CURRENT server position (lag comp disabled). */
  private currentTargets(shooter: ServerPlayer): HitTarget[] {
    return this.players
      .filter((p) => p.id !== shooter.id)
      .map((p) => ({ player: p, x: p.body.x, y: p.body.y, z: p.body.z }));
  }

  /** Append this tick's positions to the lag-comp history and drop frames older
   *  than ~1s (comfortably past MAX_REWIND_MS). DORMANT — see step(). */
  private recordHistory(now: number): void {
    this.history.push({
      t: now,
      pos: this.players.map((p) => ({ id: p.id, x: p.body.x, y: p.body.y, z: p.body.z })),
    });
    const cutoff = now - 1000;
    while (this.history.length > 2 && this.history[0].t < cutoff) this.history.shift();
  }

  /** Targets for `shooter`'s hitscan, rewound to where they were on the shooter's
   *  screen: now − (interpolation delay + the shooter's one-way latency), capped
   *  at MAX_REWIND_MS. Returns the opponent(s) at the interpolated past position;
   *  falls back to the current body when there's no history to rewind into. */
  private rewoundTargets(shooter: ServerPlayer, now: number): HitTarget[] {
    const others = this.players.filter((p) => p.id !== shooter.id);
    const cur = (): HitTarget[] => others.map((p) => ({ player: p, x: p.body.x, y: p.body.y, z: p.body.z }));
    const h = this.history;
    if (h.length === 0) return cur();

    const t = now - Math.min(MAX_REWIND_MS, INTERP_DELAY_MS + shooter.rttMs / 2);
    if (t >= h[h.length - 1].t) return cur(); // nothing to rewind into yet

    // newest-bracketing pair (or clamp to the oldest frame we still have)
    let a = h[0];
    let b = h[0];
    if (t <= h[0].t) {
      a = b = h[0];
    } else {
      for (let i = 0; i < h.length - 1; i++) {
        if (t >= h[i].t && t <= h[i + 1].t) {
          a = h[i];
          b = h[i + 1];
          break;
        }
      }
    }
    const k = b.t > a.t ? (t - a.t) / (b.t - a.t) : 0;
    return others.map((p) => {
      const pa = a.pos.find((q) => q.id === p.id);
      const pb = b.pos.find((q) => q.id === p.id);
      if (!pa || !pb) return { player: p, x: p.body.x, y: p.body.y, z: p.body.z };
      return { player: p, x: pa.x + (pb.x - pa.x) * k, y: pa.y + (pb.y - pa.y) * k, z: pa.z + (pb.z - pa.z) * k };
    });
  }

  private snapshot(now: number): GameSnapshot {
    return {
      matchId: this.id,
      tick: this.tick,
      phase: this.phase,
      timeLeftMs:
        this.phase === "STARTING"
          ? MATCH_DURATION_MS
          : this.phase === "SUDDEN_DEATH"
            ? 0
            : Math.max(0, this.endsAt - now),
      players: this.players.map((p) => p.toState()),
      shots: this.shots,
      pickups: this.pickups.map((p) => p.active),
      barrels: [...this.barrelIntact],
      booms: this.booms,
    };
  }

  private finish(winnerId: string | null, reason: MatchResult["reason"]): void {
    if (this.phase === "COMPLETED") return;
    this.phase = "COMPLETED";
    if (this.timer) clearInterval(this.timer);
    if (this.graceTimer) {
      clearTimeout(this.graceTimer);
      this.graceTimer = null;
    }

    const result = finalizeResult({
      matchId: this.id,
      players: this.players.map((p) => ({
        id: p.id,
        name: p.name,
        kills: p.kills,
        deaths: p.deaths,
      })),
      winnerId,
      reason,
      startedAt: this.startedAt,
      endedAt: Date.now(),
    });

    // Flush the replay (best-effort; a write failure must never affect payout).
    if (this.recording && this.replayFrames.length > 0) {
      void writeReplay(
        {
          matchId: this.id,
          mapSeed: this.arena.seed,
          countdownMs: COUNTDOWN_MS,
          players: this.players.map((p) => ({ id: p.id, name: p.name })),
          startedAt: this.startedAt,
          endedAt: result.endedAt,
        },
        this.replayFrames,
        result,
      ).catch(() => {});
    }
    const persist = (settlementSig: string | null) =>
      recordMatch({
        result,
        wager: this.wager,
        settlementSig,
        players: this.players.map((p) => ({
          id: p.id,
          name: p.name,
          wallet: p.wagerAddress,
          kills: p.kills,
        })),
      });

    // Wagered duel: sign keccak256(matchId, winner) with the settlement key.
    // Preferred path — the server RELAYS settle() so the pot is pushed to the
    // winner automatically (no claim tx, no gas on their side). The signature
    // still rides along in match_end so the client can fall back to
    // submitResult + claimPayout if no relayer is configured or the relay fails.
    const winner = this.players.find((p) => p.id === winnerId);
    if (this.wager && winner?.wagerAddress) {
      const terms = this.wager;
      const winnerAddress = winner.wagerAddress;
      void signSettlement(terms.matchIdHex, winnerAddress)
        .then(async (signature) => {
          persist(signature);
          let payout: "sent" | "failed" = "failed";
          if (canRelaySettlement()) {
            try {
              await relaySettlement(terms.matchIdHex, winnerAddress, signature);
              payout = "sent";
            } catch {
              payout = "failed"; // client falls back to manual claim
            }
          }
          this.broadcast({
            type: "match_end",
            result,
            settlement: { matchIdHex: terms.matchIdHex, winnerAddress, signature, payout },
          });
        })
        .catch(() => {
          persist(null);
          this.broadcast({ type: "match_end", result });
        });
    } else {
      persist(null);
      this.broadcast({ type: "match_end", result });
    }
    this.onEnd(this);
  }

  private broadcast(msg: ServerMessage): void {
    const json = JSON.stringify(msg);
    for (const p of this.players) p.send(json);
  }
}
