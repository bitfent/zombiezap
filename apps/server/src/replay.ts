// Match replays: persist the (delta-compressed) snapshot stream + metadata so a
// duel can be re-watched or audited later. For a WAGERED game this is core, not
// a nicety — it's the verifiable record behind a disputed settlement, the
// anti-cheat review trail, and the shareable highlight.
//
// Budget note: recording buffers a match's frames in RAM (~90 KB for a 2-min
// duel) then flushes to disk on completion. To stay inside the 500 MB / 5k-duel
// target we record by DEFAULT only wagered duels (low volume, high value); free
// duels record only when RECORD_REPLAYS=1. See Match.ts for the gate.

import { mkdir, writeFile, readFile } from "node:fs/promises";
import { join } from "node:path";
import type { MatchResult } from "@shotante/shared";

const REPLAY_DIR = process.env.REPLAY_DIR ?? join(process.cwd(), "replays");
const REPLAY_VERSION = 1;

export interface ReplayMeta {
  matchId: string;
  mapSeed: string;
  countdownMs: number;
  players: { id: string; name: string }[];
  startedAt: number;
  endedAt: number;
}

/** One recorded frame: ms since record start + the raw encoded snapshot bytes. */
export interface ReplayFrame {
  t: number;
  bytes: Uint8Array;
}

let dirReady = false;
async function ensureDir(): Promise<void> {
  if (dirReady) return;
  await mkdir(REPLAY_DIR, { recursive: true });
  dirReady = true;
}

/** Flush a finished match to `<REPLAY_DIR>/<matchId>.json`. Frames are base64'd
 *  so the file is portable/serveable as JSON. Best-effort — a write failure must
 *  never affect the match result, so callers fire-and-forget. */
export async function writeReplay(meta: ReplayMeta, frames: ReplayFrame[], result: MatchResult): Promise<void> {
  await ensureDir();
  const doc = {
    v: REPLAY_VERSION,
    ...meta,
    result,
    frames: frames.map((f) => ({ t: f.t, b: Buffer.from(f.bytes).toString("base64") })),
  };
  await writeFile(join(REPLAY_DIR, `${meta.matchId}.json`), JSON.stringify(doc));
}

/** Raw replay JSON for the HTTP endpoint, or null if there's no such replay. */
export async function readReplay(matchId: string): Promise<string | null> {
  try {
    return await readFile(join(REPLAY_DIR, `${matchId}.json`), "utf8");
  } catch {
    return null;
  }
}
