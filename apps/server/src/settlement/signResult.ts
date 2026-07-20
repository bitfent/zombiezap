// Result hashing + (Phase 4) signing.
//
// TODAY: produces the canonical result hash the contract will verify.
// PHASE 4: an authorized server key signs `resultHash` (EIP-191 personal_sign
// over the 32-byte hash); MatchEscrow.submitResult() checks ecrecover ==
// resultSigner and releases the pot (see contracts/src/MatchEscrow.sol).
// The signing key must live in a secret store, never in the repo.

import { createHash } from "node:crypto";
import type { MatchResult } from "@shotante/shared";

export function hashResult(result: Omit<MatchResult, "resultHash">): string {
  // Canonical JSON: fixed key order via explicit object construction.
  const canonical = JSON.stringify({
    matchId: result.matchId,
    players: result.players.map((p) => ({
      id: p.id,
      name: p.name,
      kills: p.kills,
      deaths: p.deaths,
    })),
    winnerId: result.winnerId,
    reason: result.reason,
    startedAt: result.startedAt,
    endedAt: result.endedAt,
  });
  return createHash("sha256").update(canonical).digest("hex");
}

export function finalizeResult(partial: Omit<MatchResult, "resultHash">): MatchResult {
  return { ...partial, resultHash: hashResult(partial) };
}
