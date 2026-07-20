// Wire messages (JSON over WebSocket). Discriminated unions so both sides
// exhaustively switch on `type` and the compiler catches missing handlers.
//
// Vocabulary: a CHALLENGE is a persistent, shareable game offer (it outlives
// any single connection — the durable layer). A LOBBY is the live presence of
// players inside a challenge (ephemeral — gone on disconnect). Chat is
// ephemeral by design: relayed, never stored.

import type { GameSnapshot, MatchResult, PlayerInput } from "./types.ts";
import type { WagerTerms } from "./escrow.ts";

export type ChallengeStatus =
  | "open" // waiting for an opponent
  | "matched" // opponent slot filled (reserved or joined), not started
  | "live" // duel in progress
  | "completed"
  | "cancelled"
  | "expired";

/** Public shape of a challenge — what the board and invite landing show. */
export interface ChallengeInfo {
  code: string;
  hostName: string;
  guestName: string | null;
  wagered: boolean;
  token: string | null; // NATIVE or ERC-20 address (null = free)
  wagerWei: string | null; // per-player ante (null = free, or ETH tiers priced at start)
  potWei: string | null; // 2x ante (gross pot, display)
  usdCents: number | null; // USD tier — the stable display value for fixed-stake games
  wagerTokenKind: "usdc" | "eth" | null; // which token the ante is in
  feeBps: number | null; // protocol fee in basis points (null = free game)
  isPublic: boolean;
  scheduledAt: number | null; // ms epoch; null = play now
  status: ChallengeStatus;
  ageSeconds: number;
  expiresAt: number; // ms epoch — when this DeathMatch self-expires
}

export interface LobbySlot {
  name: string;
  present: boolean; // currently connected to the lobby
  ready: boolean;
}

/** Live lobby snapshot, pushed to both occupants on every change. */
export interface LobbyState {
  code: string;
  host: LobbySlot;
  guest: LobbySlot | null;
  wagered: boolean;
  wagerWei: string | null;
  usdCents: number | null; // USD tier (stable display value)
  wagerTokenKind: "usdc" | "eth" | null;
  feeBps: number | null; // protocol fee in basis points (null = free game)
  isPublic: boolean;
  scheduledAt: number | null;
  startsInMs: number | null; // ms until the scheduled window opens (null = play now)
  expiresAt: number; // ms epoch — when this DeathMatch self-expires
}

export type ClientMessage =
  // quick match (anonymous queue). wagerAddress present = wagered queue;
  // wagerTier (USD cents) + wagerToken pick the stake.
  | { type: "queue"; name: string; wagerAddress?: string; wagerTier?: number; wagerToken?: "usdc" | "eth" }
  | { type: "leave_queue" }
  // challenges: create one and share the code/link; it survives disconnects
  | {
      type: "create_challenge";
      name: string;
      isPublic: boolean;
      scheduledAt?: number; // ms epoch — omit for play now
      wagerAddress?: string;
      wagerTier?: number; // USD cents (one of WAGER_TIERS_USD_CENTS)
      wagerToken?: "usdc" | "eth";
      reminderEmail?: string; // stored for the phase-2 reminder sweeper
    }
  // join now (or fill the opponent slot of a scheduled challenge)
  | { type: "join_challenge"; code: string; name: string; wagerAddress?: string }
  // explicitly reserve the opponent slot of a scheduled challenge
  | { type: "reserve_challenge"; code: string; name: string; wagerAddress?: string }
  // re-attach to your challenge after a refresh/restart (key = hostKey|guestKey).
  // wagered matches may instead prove ownership with a wallet signature over the
  // nonce issued by reclaim_nonce (no device key needed — cross-device rejoin).
  | { type: "reclaim_challenge"; code: string; key: string; name: string; wagerAddress?: string; signature?: string }
  // ask for a one-time nonce to sign for wallet-based reclaim (wagered only).
  // the server derives which slot (host/guest) from the connected wallet.
  | { type: "reclaim_nonce_request"; code: string; wagerAddress: string }
  | { type: "lobby_ready"; ready: boolean }
  // step out of a lobby without cancelling it — presence drops, challenge lives
  | { type: "leave_lobby" }
  | { type: "cancel_challenge"; code: string; hostKey: string }
  // board: live subscription to public upcoming challenges (no polling)
  | { type: "sub_board" }
  | { type: "unsub_board" }
  // ephemeral lobby text chat (in-match proximity chat is now VOICE, sent as
  // binary frames — see Voice.ts; a text chat received during a match is dropped)
  | { type: "chat"; text: string }
  | { type: "input"; input: PlayerInput }
  // RTT probe: echo the server's ping timestamp straight back so the server can
  // measure round-trip time (drives lag-compensated hit registration)
  | { type: "pong"; t: number }
  // reconnect to a live match after a drop, using the per-match token
  | { type: "rejoin_match"; token: string };

export type ServerMessage =
  | { type: "welcome"; playerId: string; protocol: number }
  | { type: "queued"; wagered: boolean }
  | { type: "challenge_created"; code: string; hostKey: string; info: ChallengeInfo }
  | { type: "challenge_reserved"; code: string; guestKey: string; info: ChallengeInfo }
  // one-time message to sign for wallet-based reclaim (wagered only)
  | { type: "reclaim_nonce"; code: string; role: "host" | "guest"; message: string }
  | { type: "lobby_state"; lobby: LobbyState }
  | { type: "challenge_cancelled"; code: string; reason: "cancelled" | "expired" }
  | { type: "board_update"; upcoming: ChallengeInfo[] }
  | { type: "chat"; from: string; text: string; scope: "lobby" }
  | {
      // Escrow handshake (wagered duels only): the server tells each player
      // which transaction to send. p1 gets "create", p2 gets "join" once the
      // chain shows the match Created.
      type: "escrow_action";
      action: "create" | "join";
      terms: WagerTerms;
    }
  | {
      type: "escrow_status";
      matchIdHex: string;
      status: "waiting" | "locked" | "failed";
      detail?: string;
    }
  | {
      type: "match_start";
      matchId: string;
      countdownMs: number;
      players: { id: string; name: string }[];
      wagered: boolean;
      // Both sides regenerate the SAME arena from this seed (mapgen.ts).
      mapSeed: string;
    }
  | { type: "snapshot"; snapshot: GameSnapshot }
  // RTT probe — the client echoes `t` back in a pong (see lag compensation)
  | { type: "ping"; t: number }
  // Reconnect support: `match_token` is each player's private key to re-enter the
  // live match after a drop. When one player drops, the duel PAUSES — the stayer
  // gets `opponent_dropped` (with the grace window) then `opponent_reconnected`
  // or a `match_end`; the returning player gets `match_resumed`.
  | { type: "match_token"; token: string }
  | { type: "opponent_dropped"; graceMs: number }
  | { type: "opponent_reconnected" }
  | { type: "match_resumed" }
  | {
      type: "match_end";
      result: MatchResult;
      // Wagered matches: the server's EIP-191 signature over
      // keccak256(matchId, winner). Normally the server relays settle() itself
      // (payout pushed to the winner — no client tx). `payout` reports that:
      //   "sent"    — settle() landed on-chain, pot already in the winner's wallet
      //   "pending" — relay in flight (rare; client may wait or fall back)
      //   "failed" / undefined — no relayer or relay failed; the winner can use
      //     the signature to submitResult + claimPayout themselves (fallback).
      settlement?: {
        matchIdHex: string;
        winnerAddress: string;
        signature: string;
        payout?: "sent" | "pending" | "failed";
      };
    }
  | { type: "error"; message: string };

const CLIENT_TYPES = new Set([
  "queue",
  "leave_queue",
  "create_challenge",
  "join_challenge",
  "reserve_challenge",
  "reclaim_challenge",
  "reclaim_nonce_request",
  "lobby_ready",
  "leave_lobby",
  "cancel_challenge",
  "sub_board",
  "unsub_board",
  "chat",
  "input",
  "pong",
  "rejoin_match",
]);

export function parseClientMessage(raw: string): ClientMessage | null {
  try {
    const m = JSON.parse(raw);
    if (m && CLIENT_TYPES.has(m.type)) return m as ClientMessage;
  } catch {
    /* malformed input is just dropped */
  }
  return null;
}
