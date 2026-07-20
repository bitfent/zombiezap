// Matchmaking, two ways in:
//   QUICK MATCH — anonymous queues (casual + wagered, never mixed)
//   CHALLENGES  — persistent player-created games: private (invite code/link)
//                 or public (live board). A challenge is DURABLE (in-memory map
//                 + best-effort Postgres) and survives the host closing the
//                 tab; the LOBBY (who is connected, ready flags, chat) is
//                 ephemeral RAM, rebuilt as players (re)attach via their keys.
//
// Start trigger: both occupants present AND both ready AND (no schedule OR the
// scheduled time has arrived). Play-now lobbies auto-ready on attach so a join
// with both present still starts instantly. Wagered challenges run the escrow
// handshake AT START TIME — nobody's funds are locked while a scheduled match
// waits for its window.
//
// Wagered flow (spec §10 lifecycle, escrow slice):
//   pair -> ESCROW: tell p1 "create" -> poll chain -> Created: tell p2 "join"
//   -> poll -> Locked (and the on-chain p1/p2 match the claimed wallets)
//   -> start the duel. Timeout or mismatch -> failed, both informed, no match.

import { randomUUID } from "node:crypto";
import { Match } from "../game/Match.ts";
import type { ServerPlayer } from "../game/Player.ts";
import {
  escrowFeeBps,
  newWagerTerms,
  readEscrow,
  verifyWalletSignature,
  wagerMode,
} from "../settlement/escrow.ts";
import {
  CHALLENGE_TTL_MS,
  SCHEDULE_GRACE_MS,
  SCHEDULE_MAX_AHEAD_MS,
  WAGER_TIERS_USD_CENTS,
  type ChallengeInfo,
  type ChallengeStatus,
  type LobbyState,
  type WagerTerms,
} from "@shotante/shared";
import {
  updateChallengeStatus,
  upsertChallenge,
  type ChallengeRecord,
} from "../db.ts";

const ESCROW_POLL_MS = 2500;
const ESCROW_TIMEOUT_MS = 120_000;
const SWEEP_MS = 5_000; // expiry + scheduled-start sweeper
const RECLAIM_NONCE_TTL_MS = 2 * 60_000; // a sign-to-reclaim nonce is short-lived
// Unambiguous challenge-code alphabet (no 0/O/1/I).
const CODE_CHARS = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

interface PendingEscrow {
  terms: WagerTerms;
  p1: ServerPlayer;
  p2: ServerPlayer;
  deadline: number;
  joinSent: boolean;
  timer: ReturnType<typeof setInterval>;
  challengeCode: string | null;
}

/** Ephemeral lobby presence — who is attached to a challenge right now. */
interface Lobby {
  host: ServerPlayer | null;
  guest: ServerPlayer | null;
  hostReady: boolean;
  guestReady: boolean;
}

/** Clamp a client-supplied stake to a known tier (defaults to $1). */
function validTier(cents: number | null | undefined): number {
  return cents != null && (WAGER_TIERS_USD_CENTS as readonly number[]).includes(cents) ? cents : 100;
}

export class DuelQueue {
  private casual: ServerPlayer[] = [];
  private wagered: ServerPlayer[] = [];
  private challenges = new Map<string, ChallengeRecord>(); // code -> challenge
  private lobbies = new Map<string, Lobby>(); // code -> live presence
  private lobbyOf = new Map<string, string>(); // playerId -> code
  private boardSubs = new Map<string, ServerPlayer>(); // playerId -> socket
  private active = new Map<string, Match>(); // playerId -> match
  private rejoinTokens = new Map<string, { match: Match; player: ServerPlayer }>(); // token -> live-match re-entry
  private pending = new Map<string, PendingEscrow>(); // playerId -> handshake
  // wallet-based reclaim: one-time nonce-to-sign keyed by `${code}:${role}`
  private reclaimNonces = new Map<string, { message: string; expires: number }>();

  constructor() {
    setInterval(() => this.sweep(), SWEEP_MS);
  }

  /** Boot-time restore: challenges loaded from Postgres (no presence yet). */
  restore(records: ChallengeRecord[]): void {
    const now = Date.now();
    for (const r of records) {
      if (r.expiresAt > now) this.challenges.set(r.code, r);
    }
    if (this.challenges.size > 0) {
      console.log(`challenges: restored ${this.challenges.size} open from db`);
    }
  }

  // ── quick match ──────────────────────────────────────────────────────────
  enqueue(player: ServerPlayer): void {
    if (this.isBusy(player.id)) return;
    const wantsWager = player.wagerAddress !== null;
    if (wantsWager) {
      const tier = validTier(player.wagerTier);
      const kind = player.wagerTokenKind ?? "eth";
      player.wagerTier = tier;
      player.wagerTokenKind = kind;
      // only pair players who picked the SAME stake (tier + token)
      const i = this.wagered.findIndex((p) => p.wagerTier === tier && p.wagerTokenKind === kind);
      if (i >= 0) {
        const b = this.wagered.splice(i, 1)[0];
        this.startPair(player, b, true, null, tier, kind);
        return;
      }
      this.wagered.push(player);
      player.send(JSON.stringify({ type: "queued", wagered: true }));
      return;
    }
    this.casual.push(player);
    player.send(JSON.stringify({ type: "queued", wagered: false }));
    if (this.casual.length >= 2) {
      const a = this.casual.shift()!;
      const b = this.casual.shift()!;
      this.startPair(a, b, false, null, null, null);
    }
  }

  // ── challenges ───────────────────────────────────────────────────────────
  createChallenge(
    host: ServerPlayer,
    opts: { isPublic: boolean; scheduledAt?: number; reminderEmail?: string },
  ): void {
    if (this.isBusy(host.id)) return;
    const now = Date.now();
    // Scheduling is disabled for now — every DeathMatch is play-now. The opts
    // field and the validation below are kept so it can be re-enabled later.
    const scheduledAt = null as number | null;
    if (scheduledAt !== null) {
      if (!Number.isFinite(scheduledAt) || scheduledAt < now - 60_000) {
        host.send(JSON.stringify({ type: "error", message: "scheduled time is in the past" }));
        return;
      }
      if (scheduledAt > now + SCHEDULE_MAX_AHEAD_MS) {
        host.send(JSON.stringify({ type: "error", message: "you can schedule at most a week ahead" }));
        return;
      }
    }
    let code = "";
    do {
      code = Array.from({ length: 5 }, () => CODE_CHARS[Math.floor(Math.random() * CODE_CHARS.length)]).join("");
    } while (this.challenges.has(code));

    const wagered = host.wagerAddress !== null;
    // Fixed-stake wager: store the USD tier + token; the on-chain wei is resolved
    // at match start (newWagerTerms — ETH is priced then). Display uses usdCents.
    const ch: ChallengeRecord = {
      code,
      hostKey: randomUUID(),
      hostName: host.name,
      hostWallet: host.wagerAddress,
      guestName: null,
      guestWallet: null,
      guestKey: null,
      wagered,
      token: null,
      wagerWei: null,
      usdCents: wagered ? validTier(host.wagerTier) : null,
      wagerTokenKind: wagered ? (host.wagerTokenKind ?? "eth") : null,
      isPublic: opts.isPublic,
      scheduledAt,
      status: "open",
      createdAt: now,
      expiresAt: scheduledAt !== null ? scheduledAt + SCHEDULE_GRACE_MS : now + CHALLENGE_TTL_MS,
      reminderEmail: opts.reminderEmail?.slice(0, 200) ?? null,
    };
    this.challenges.set(code, ch);
    upsertChallenge(ch);
    host.send(JSON.stringify({ type: "challenge_created", code, hostKey: ch.hostKey, info: this.info(ch) }));
    this.attach(ch, "host", host);
    this.broadcastLobby(code);
    if (ch.isPublic) this.pushBoard();
  }

  /** Join (play now) or fill the opponent slot (scheduled). One path: a join
   *  on a scheduled challenge IS a reservation — the duel fires at the time. */
  joinChallenge(joiner: ServerPlayer, code: string): void {
    if (this.isBusy(joiner.id)) return;
    const ch = this.challenges.get(code.toUpperCase());
    if (!ch || ch.status === "expired" || ch.status === "cancelled" || ch.status === "completed") {
      joiner.send(JSON.stringify({ type: "error", message: "challenge not found — it may have finished or expired" }));
      return;
    }
    if (ch.status === "live") {
      joiner.send(JSON.stringify({ type: "error", message: "that duel is already underway" }));
      return;
    }
    const lobby = this.lobbies.get(ch.code);
    if (lobby?.host?.id === joiner.id) {
      joiner.send(JSON.stringify({ type: "error", message: "that's your own challenge" }));
      return;
    }
    if (ch.status === "matched") {
      joiner.send(
        JSON.stringify({ type: "error", message: "this challenge already has an opponent — open your own invite link to rejoin" }),
      );
      return;
    }
    if (ch.wagered && joiner.wagerAddress === null) {
      joiner.send(JSON.stringify({ type: "error", message: "this is a wagered challenge — connect a wallet to join" }));
      return;
    }
    if (!ch.wagered && joiner.wagerAddress !== null) joiner.wagerAddress = null; // free challenge: ignore wallet

    ch.guestName = joiner.name;
    ch.guestWallet = joiner.wagerAddress;
    ch.guestKey = randomUUID();
    ch.status = "matched";
    upsertChallenge(ch);
    joiner.send(JSON.stringify({ type: "challenge_reserved", code: ch.code, guestKey: ch.guestKey, info: this.info(ch) }));
    this.attach(ch, "guest", joiner);
    this.broadcastLobby(ch.code);
    if (ch.isPublic) this.pushBoard();
    this.checkStart(ch.code);
  }

  /** Issue a one-time message for wallet-based reclaim (wagered matches only).
   *  The client signs it with the wallet bound to that slot; we verify the
   *  signature on reclaim. Lets a wagered player rejoin from a new device with
   *  no stored key — the wallet is the portable identity. */
  requestReclaimNonce(p: ServerPlayer, code: string, wagerAddress: string): void {
    const ch = this.challenges.get(code.toUpperCase());
    if (!ch || !ch.wagered || (ch.status !== "open" && ch.status !== "matched")) {
      p.send(JSON.stringify({ type: "error", message: "challenge not found — it may have finished or expired" }));
      return;
    }
    const w = wagerAddress.toLowerCase();
    const role: "host" | "guest" | null =
      ch.hostWallet?.toLowerCase() === w ? "host" : ch.guestWallet?.toLowerCase() === w ? "guest" : null;
    if (!role) {
      p.send(JSON.stringify({ type: "error", message: "this wallet isn't part of that DeathMatch" }));
      return;
    }
    const nonce = randomUUID();
    const message = `ShotAnte DeathMatch reclaim\ncode: ${ch.code}\nrole: ${role}\nnonce: ${nonce}`;
    this.reclaimNonces.set(`${ch.code}:${role}`, { message, expires: Date.now() + RECLAIM_NONCE_TTL_MS });
    p.send(JSON.stringify({ type: "reclaim_nonce", code: ch.code, role, message }));
  }

  /** Verify a wallet signature against an outstanding reclaim nonce. Returns the
   *  matched role (and consumes the nonce) or null. Smart-wallet-safe. */
  private async verifyReclaimSignature(
    ch: ChallengeRecord,
    signature: string,
  ): Promise<"host" | "guest" | null> {
    const now = Date.now();
    for (const role of ["host", "guest"] as const) {
      const wallet = role === "host" ? ch.hostWallet : ch.guestWallet;
      if (!wallet) continue;
      const entry = this.reclaimNonces.get(`${ch.code}:${role}`);
      if (!entry || entry.expires < now) continue;
      if (await verifyWalletSignature(wallet, entry.message, signature)) {
        this.reclaimNonces.delete(`${ch.code}:${role}`);
        return role;
      }
    }
    return null;
  }

  /** Re-attach to a challenge after refresh/restart. key = hostKey | guestKey,
   *  OR (wagered, cross-device) a wallet signature over the reclaim nonce. */
  async reclaimChallenge(p: ServerPlayer, code: string, key: string, signature?: string): Promise<void> {
    if (this.isBusy(p.id)) return;
    const ch = this.challenges.get(code.toUpperCase());
    if (!ch || (ch.status !== "open" && ch.status !== "matched")) {
      p.send(JSON.stringify({ type: "error", message: "challenge not found — it may have finished or expired" }));
      return;
    }
    let role: "host" | "guest" | null = key === ch.hostKey ? "host" : key === ch.guestKey ? "guest" : null;
    let mintedKey: string | null = null;
    if (!role && signature && ch.wagered) {
      role = await this.verifyReclaimSignature(ch, signature);
      if (role) {
        // The device proved wallet control; issue it a fresh key for the fast
        // path next time, superseding any key the original device held.
        mintedKey = randomUUID();
        if (role === "host") ch.hostKey = mintedKey;
        else ch.guestKey = mintedKey;
        upsertChallenge(ch);
      }
    }
    if (!role) {
      p.send(JSON.stringify({ type: "error", message: "that invite key doesn't match this challenge" }));
      return;
    }
    if (ch.wagered && p.wagerAddress === null) {
      p.send(JSON.stringify({ type: "error", message: "this is a wagered challenge — connect a wallet to rejoin" }));
      return;
    }
    if (!ch.wagered) p.wagerAddress = null;
    // Wallet may legitimately change between sessions — the challenge follows it.
    if (role === "host" && ch.hostWallet !== p.wagerAddress) {
      ch.hostWallet = p.wagerAddress;
      upsertChallenge(ch);
    } else if (role === "guest" && ch.guestWallet !== p.wagerAddress) {
      ch.guestWallet = p.wagerAddress;
      upsertChallenge(ch);
    }
    // Hand a wallet-reclaiming device its fresh key (reuses the create/reserve
    // messages the client already stores keys from).
    if (mintedKey) {
      p.send(
        JSON.stringify(
          role === "host"
            ? { type: "challenge_created", code: ch.code, hostKey: mintedKey, info: this.info(ch) }
            : { type: "challenge_reserved", code: ch.code, guestKey: mintedKey, info: this.info(ch) },
        ),
      );
    }
    this.attach(ch, role, p);
    this.broadcastLobby(ch.code);
    this.checkStart(ch.code);
  }

  lobbyReady(playerId: string, ready: boolean): void {
    const code = this.lobbyOf.get(playerId);
    const lobby = code ? this.lobbies.get(code) : undefined;
    if (!code || !lobby) return;
    if (lobby.host?.id === playerId) lobby.hostReady = ready;
    else if (lobby.guest?.id === playerId) lobby.guestReady = ready;
    this.broadcastLobby(code);
    this.checkStart(code);
  }

  /** Step out of a lobby without ending it: presence drops, challenge lives.
   *  Used by guest "leave" and by host BACK on a scheduled-style detach. */
  leaveLobby(playerId: string): void {
    this.detachLobby(playerId);
  }

  cancelChallenge(playerId: string, code: string, hostKey: string): void {
    const ch = this.challenges.get(code.toUpperCase());
    if (!ch || ch.hostKey !== hostKey) return;
    if (ch.status !== "open" && ch.status !== "matched") return;
    this.closeChallenge(ch, "cancelled");
  }

  /** Ephemeral lobby chat: relayed to the other occupant, never stored. */
  lobbyChat(playerId: string, from: string, text: string): void {
    const code = this.lobbyOf.get(playerId);
    const lobby = code ? this.lobbies.get(code) : undefined;
    if (!lobby) return;
    const other = lobby.host?.id === playerId ? lobby.guest : lobby.host;
    other?.send(JSON.stringify({ type: "chat", from, text, scope: "lobby" }));
  }

  // ── board (live subscription, replaces list polling) ────────────────────
  subBoard(p: ServerPlayer): void {
    this.boardSubs.set(p.id, p);
    p.send(JSON.stringify({ type: "board_update", upcoming: this.upcoming() }));
  }

  unsubBoard(playerId: string): void {
    this.boardSubs.delete(playerId);
  }

  /** Public upcoming challenges: soonest scheduled first, then newest open. */
  upcoming(): ChallengeInfo[] {
    const all = [...this.challenges.values()].filter(
      (c) => c.isPublic && (c.status === "open" || c.status === "matched"),
    );
    const scheduled = all
      .filter((c) => c.scheduledAt !== null)
      .sort((a, b) => a.scheduledAt! - b.scheduledAt!);
    const now = all.filter((c) => c.scheduledAt === null).sort((a, b) => b.createdAt - a.createdAt);
    return [...scheduled, ...now].slice(0, 50).map((c) => this.info(c));
  }

  challengeInfo(code: string): ChallengeInfo | null {
    const ch = this.challenges.get(code.toUpperCase());
    return ch ? this.info(ch) : null;
  }

  private pushBoard(): void {
    const json = JSON.stringify({ type: "board_update", upcoming: this.upcoming() });
    for (const p of this.boardSubs.values()) p.send(json);
  }

  // ── lobby internals ──────────────────────────────────────────────────────
  private attach(ch: ChallengeRecord, role: "host" | "guest", p: ServerPlayer): void {
    let lobby = this.lobbies.get(ch.code);
    if (!lobby) {
      lobby = { host: null, guest: null, hostReady: false, guestReady: false };
      this.lobbies.set(ch.code, lobby);
    }
    // Play-now lobbies auto-ready: a join with both present starts instantly.
    const autoReady = ch.scheduledAt === null;
    if (role === "host") {
      lobby.host = p;
      lobby.hostReady = autoReady;
    } else {
      lobby.guest = p;
      lobby.guestReady = autoReady;
    }
    this.lobbyOf.set(p.id, ch.code);
  }

  private detachLobby(playerId: string): void {
    const code = this.lobbyOf.get(playerId);
    if (!code) return;
    this.lobbyOf.delete(playerId);
    const lobby = this.lobbies.get(code);
    if (!lobby) return;
    if (lobby.host?.id === playerId) {
      lobby.host = null;
      lobby.hostReady = false;
    } else if (lobby.guest?.id === playerId) {
      lobby.guest = null;
      lobby.guestReady = false;
    }
    if (!lobby.host && !lobby.guest) this.lobbies.delete(code); // challenge survives
    else this.broadcastLobby(code);
  }

  private lobbyState(ch: ChallengeRecord): LobbyState {
    const lobby = this.lobbies.get(ch.code);
    return {
      code: ch.code,
      host: { name: ch.hostName, present: !!lobby?.host, ready: !!lobby?.hostReady },
      guest:
        ch.guestName !== null
          ? { name: ch.guestName, present: !!lobby?.guest, ready: !!lobby?.guestReady }
          : null,
      wagered: ch.wagered,
      wagerWei: ch.wagerWei,
      usdCents: ch.usdCents,
      wagerTokenKind: ch.wagerTokenKind,
      feeBps: ch.wagered ? escrowFeeBps() : null,
      isPublic: ch.isPublic,
      scheduledAt: ch.scheduledAt,
      startsInMs: ch.scheduledAt !== null ? Math.max(0, ch.scheduledAt - Date.now()) : null,
      expiresAt: ch.expiresAt,
    };
  }

  private broadcastLobby(code: string): void {
    const ch = this.challenges.get(code);
    const lobby = this.lobbies.get(code);
    if (!ch || !lobby) return;
    const json = JSON.stringify({ type: "lobby_state", lobby: this.lobbyState(ch) });
    lobby.host?.send(json);
    lobby.guest?.send(json);
  }

  private checkStart(code: string): void {
    const ch = this.challenges.get(code);
    const lobby = this.lobbies.get(code);
    if (!ch || !lobby || (ch.status !== "open" && ch.status !== "matched")) return;
    if (!lobby.host || !lobby.guest) return;
    if (!lobby.hostReady || !lobby.guestReady) return;
    if (ch.scheduledAt !== null && Date.now() < ch.scheduledAt) return;
    if (ch.wagered && (lobby.host.wagerAddress === null || lobby.guest.wagerAddress === null)) return;

    const host = lobby.host;
    const guest = lobby.guest;
    ch.status = "live";
    updateChallengeStatus(ch.code, "live");
    this.lobbies.delete(code);
    this.lobbyOf.delete(host.id);
    this.lobbyOf.delete(guest.id);
    if (ch.isPublic) this.pushBoard();
    this.startPair(host, guest, ch.wagered, ch.code, ch.usdCents, ch.wagerTokenKind);
  }

  /** Terminal transition: notify occupants, drop presence, persist status. */
  private closeChallenge(ch: ChallengeRecord, status: "cancelled" | "expired"): void {
    ch.status = status;
    updateChallengeStatus(ch.code, status);
    const lobby = this.lobbies.get(ch.code);
    const json = JSON.stringify({ type: "challenge_cancelled", code: ch.code, reason: status });
    lobby?.host?.send(json);
    lobby?.guest?.send(json);
    if (lobby?.host) this.lobbyOf.delete(lobby.host.id);
    if (lobby?.guest) this.lobbyOf.delete(lobby.guest.id);
    this.lobbies.delete(ch.code);
    this.challenges.delete(ch.code);
    if (ch.isPublic) this.pushBoard();
  }

  private finishChallenge(code: string | null, status: ChallengeStatus): void {
    if (!code) return;
    const ch = this.challenges.get(code);
    if (!ch) return;
    ch.status = status;
    updateChallengeStatus(code, status);
    this.challenges.delete(code);
  }

  /** Expiry + scheduled-start timer. One interval owns all time-based moves. */
  private sweep(): void {
    const now = Date.now();
    for (const ch of [...this.challenges.values()]) {
      if (ch.status !== "open" && ch.status !== "matched") continue;
      if (now > ch.expiresAt) {
        this.closeChallenge(ch, "expired");
      } else if (ch.scheduledAt !== null && now >= ch.scheduledAt) {
        this.checkStart(ch.code); // fires the lobby when its window opens
      }
    }
  }

  // ── pairing ──────────────────────────────────────────────────────────────
  private startPair(
    a: ServerPlayer,
    b: ServerPlayer,
    wagered: boolean,
    challengeCode: string | null,
    usdCents: number | null,
    tokenKind: "usdc" | "eth" | null,
  ): void {
    if (wagered && usdCents != null && tokenKind != null) {
      void this.startEscrow(a, b, challengeCode, usdCents, tokenKind);
    } else {
      this.startMatch(a, b, null, challengeCode);
    }
  }

  private startMatch(a: ServerPlayer, b: ServerPlayer, terms: WagerTerms | null, challengeCode: string | null): void {
    const tokens = [randomUUID(), randomUUID()];
    const match = new Match(a, b, terms, (m) => {
      for (const id of m.playerIds) this.active.delete(id);
      for (const t of tokens) this.rejoinTokens.delete(t); // match over — invalidate re-entry
      this.finishChallenge(challengeCode, "completed");
    });
    this.active.set(a.id, match);
    this.active.set(b.id, match);
    // each player gets a private token to re-enter THIS match after a drop
    [a, b].forEach((p, i) => {
      this.rejoinTokens.set(tokens[i], { match, player: p });
      p.send(JSON.stringify({ type: "match_token", token: tokens[i] }));
    });
  }

  /** Re-enter a live match after a drop. Rebinds the existing player to the new
   *  socket and unpauses the duel. Returns the (reused) ServerPlayer, or null if
   *  the token is unknown / the match already ended. */
  rejoinMatch(
    token: string,
    send: (json: string) => void,
    sendBinary: (data: Buffer | ArrayBuffer | Uint8Array) => void,
  ): ServerPlayer | null {
    const entry = this.rejoinTokens.get(token);
    if (!entry || !this.active.has(entry.player.id)) return null;
    entry.player.send = send;
    entry.player.sendBinary = sendBinary;
    if (!entry.match.reconnect(entry.player.id)) return null;
    return entry.player;
  }

  private async startEscrow(
    p1: ServerPlayer,
    p2: ServerPlayer,
    challengeCode: string | null,
    usdCents: number,
    tokenKind: "usdc" | "eth",
  ): Promise<void> {
    let terms: WagerTerms;
    try {
      terms = await newWagerTerms(usdCents, tokenKind); // ETH path prices off CoinGecko
    } catch (err) {
      console.error("could not price wager", err);
      const msg = "couldn't price the wager right now — please try again";
      p1.send(JSON.stringify({ type: "error", message: msg }));
      p2.send(JSON.stringify({ type: "error", message: msg }));
      return;
    }
    const pe: PendingEscrow = {
      terms,
      p1,
      p2,
      deadline: Date.now() + ESCROW_TIMEOUT_MS,
      joinSent: false,
      timer: setInterval(() => void this.pollEscrow(pe), ESCROW_POLL_MS),
      challengeCode,
    };
    this.pending.set(p1.id, pe);
    this.pending.set(p2.id, pe);
    p1.send(JSON.stringify({ type: "escrow_action", action: "create", terms }));
    p2.send(
      JSON.stringify({
        type: "escrow_status",
        matchIdHex: terms.matchIdHex,
        status: "waiting",
        detail: "opponent is anteing in…",
      }),
    );
    void this.pollEscrow(pe);
  }

  private async pollEscrow(pe: PendingEscrow): Promise<void> {
    if (!this.pending.has(pe.p1.id)) return; // already resolved
    if (Date.now() > pe.deadline) {
      this.failEscrow(pe, "ante timeout — escrow not funded in time");
      return;
    }
    let state;
    try {
      state = await readEscrow(pe.terms.matchIdHex);
    } catch {
      return; // transient RPC error — try again next tick
    }

    if (state.status === "created" && !pe.joinSent) {
      pe.joinSent = true;
      pe.p2.send(JSON.stringify({ type: "escrow_action", action: "join", terms: pe.terms }));
      return;
    }
    if (state.status === "locked") {
      // chain mode: the wallets that anted MUST be the wallets that queued.
      if (
        wagerMode === "chain" &&
        (state.p1 !== pe.p1.wagerAddress || state.p2 !== pe.p2.wagerAddress)
      ) {
        this.failEscrow(pe, "escrow funded by unexpected wallets");
        return;
      }
      this.resolveEscrow(pe);
      const status = JSON.stringify({
        type: "escrow_status",
        matchIdHex: pe.terms.matchIdHex,
        status: "locked",
      });
      pe.p1.send(status);
      pe.p2.send(status);
      this.startMatch(pe.p1, pe.p2, pe.terms, pe.challengeCode);
    }
  }

  private resolveEscrow(pe: PendingEscrow): void {
    clearInterval(pe.timer);
    this.pending.delete(pe.p1.id);
    this.pending.delete(pe.p2.id);
  }

  private failEscrow(pe: PendingEscrow, detail: string): void {
    this.resolveEscrow(pe);
    const msg = JSON.stringify({
      type: "escrow_status",
      matchIdHex: pe.terms.matchIdHex,
      status: "failed",
      detail: `${detail} — if you anted, cancel or refund from the match screen`,
    });
    pe.p1.send(msg);
    pe.p2.send(msg);
    if (pe.challengeCode) {
      const ch = this.challenges.get(pe.challengeCode);
      if (ch) this.closeChallenge(ch, "cancelled");
    }
  }

  // ── shared bookkeeping ───────────────────────────────────────────────────
  private isBusy(playerId: string): boolean {
    return (
      this.casual.some((p) => p.id === playerId) ||
      this.wagered.some((p) => p.id === playerId) ||
      this.lobbyOf.has(playerId) ||
      this.active.has(playerId) ||
      this.pending.has(playerId)
    );
  }

  leave(playerId: string): void {
    this.casual = this.casual.filter((p) => p.id !== playerId);
    this.wagered = this.wagered.filter((p) => p.id !== playerId);
  }

  matchOf(playerId: string): Match | undefined {
    return this.active.get(playerId);
  }

  disconnect(playerId: string): void {
    this.leave(playerId);
    this.boardSubs.delete(playerId);
    this.detachLobby(playerId); // presence drops; the challenge itself survives
    const pe = this.pending.get(playerId);
    if (pe) this.failEscrow(pe, "opponent disconnected during ante");
    this.matchOf(playerId)?.handleDisconnect(playerId);
  }

  // ── helpers ──────────────────────────────────────────────────────────────
  private info(ch: ChallengeRecord): ChallengeInfo {
    return {
      code: ch.code,
      hostName: ch.hostName,
      guestName: ch.guestName,
      wagered: ch.wagered,
      token: ch.token,
      wagerWei: ch.wagerWei,
      potWei: ch.wagerWei !== null ? (BigInt(ch.wagerWei) * 2n).toString() : null,
      usdCents: ch.usdCents,
      wagerTokenKind: ch.wagerTokenKind,
      feeBps: ch.wagered ? escrowFeeBps() : null,
      isPublic: ch.isPublic,
      scheduledAt: ch.scheduledAt,
      status: ch.status,
      ageSeconds: Math.round((Date.now() - ch.createdAt) / 1000),
      expiresAt: ch.expiresAt,
    };
  }
}
