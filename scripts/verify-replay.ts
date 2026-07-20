// Decode a recorded replay file end-to-end and assert it's coherent:
//   node --experimental-strip-types scripts/verify-replay.ts <replay.json>
// Proves the recorded delta stream replays cleanly and the final standings agree
// with the signed match result (the leader at the end is the recorded winner).
import { readFileSync } from "node:fs";
import { SnapshotDecoder } from "../packages/shared/src/snapshotCodec.ts";

const path = process.argv[2];
if (!path) {
  console.error("usage: verify-replay.ts <replay.json>");
  process.exit(1);
}

const doc = JSON.parse(readFileSync(path, "utf8"));
const dec = new SnapshotDecoder();
let last: ReturnType<SnapshotDecoder["decode"]> | null = null;
for (const f of doc.frames as { t: number; b: string }[]) {
  const bytes = Buffer.from(f.b, "base64");
  last = dec.decode(bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength));
  for (const p of last.players) {
    if (!Number.isFinite(p.x) || !Number.isFinite(p.y) || !Number.isFinite(p.z)) {
      console.error("FAIL: NaN position decoded");
      process.exit(1);
    }
  }
}

if (!last || last.players.length === 0) {
  console.error("FAIL: no decodable frames");
  process.exit(1);
}

const leader = [...last.players].sort((a, b) => b.kills - a.kills)[0];
const winnerId: string | null = doc.result.winnerId;
// A kills/time/sudden_death win means the winner leads on the scoreboard. A
// forfeit/void win is decided by who stayed connected, NOT score — so for those
// we only require the stream to decode cleanly.
const scoreDecided = !["forfeit", "void"].includes(doc.result.reason);
const ok = doc.frames.length > 0 && (winnerId === null || !scoreDecided || leader.id === winnerId);

console.log(`frames=${doc.frames.length} map=${doc.mapSeed}`);
console.log(`final standings: ${last.players.map((p) => `${p.name} ${p.kills}k/${p.deaths}d`).join("  ·  ")}`);
console.log(`recorded winner=${winnerId} reason=${doc.result.reason}`);
console.log(ok ? "PASS: replay decodes cleanly and the final leader matches the recorded winner" : "FAIL: leader/winner mismatch");
process.exit(ok ? 0 : 1);
