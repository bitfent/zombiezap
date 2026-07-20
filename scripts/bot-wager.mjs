// Headless verification of the WAGERED flow (server in WAGER_MODE=mock):
// two bots queue with wallet addresses -> escrow handshake (create/join
// actions, locked status) -> duel -> match_end carries a settlement whose
// EIP-191 signature must recover to the server's settlement signer over
// keccak256(matchId, winner). Exactly what MatchEscrow.submitResult checks.
//
// Run:  WAGER_MODE=mock npm run dev:server   then   npm run verify:wager

import WebSocket from "ws";
import { encodePacked, keccak256, recoverMessageAddress } from "viem";

const URL = process.env.SERVER_URL ?? "ws://localhost:8080";
const HEALTH = URL.replace(/^ws/, "http") + "/health";

const health = await fetch(HEALTH).then((r) => r.json());
if (health.wagerMode === "off") {
  console.error("FAIL: server has WAGER_MODE=off — start it with WAGER_MODE=mock");
  process.exit(1);
}
console.log(`server wagerMode=${health.wagerMode} signer=${health.settlementSigner}`);

const deadline = setTimeout(() => {
  console.error("FAIL: wagered match did not complete within 200s");
  process.exit(1);
}, 200_000);

const seen = { create: false, join: false, locked: false, wageredStart: false };

function addr() {
  const hex = [...crypto.getRandomValues(new Uint8Array(20))]
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return `0x${hex}`;
}

// Stateful binary snapshot decoder (delta stream) — see packages/shared/snapshotCodec.
const M_X = 1, M_Y = 2, M_Z = 4, M_YAW = 8, M_HEALTH = 16, M_KILLS = 32, M_DEATHS = 64, M_ALIVE = 128;
function makeSnapshotDecoder() {
  let baseline = null;
  return (buf) => {
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    let o = 1;
    const u8 = () => dv.getUint8(o++);
    const f32 = () => { const v = dv.getFloat32(o, true); o += 4; return v; };
    const str = () => { let s = ""; for (let n = u8(), i = 0; i < n; i++) s += String.fromCharCode(u8()); return s; };
    const flags = u8();
    dv.getUint32(o, true), (o += 4); u8(); dv.getUint32(o, true), (o += 4); // tick, phase, timeLeft
    const pc = u8();
    let players;
    if (flags & 1) {
      players = [];
      for (let i = 0; i < pc; i++) {
        const id = str(), name = str();
        const x = f32(), y = f32(), z = f32(), yaw = f32();
        const health = u8(), kills = u8(), deaths = u8(), alive = u8() === 1;
        players.push({ id, name, x, y, z, yaw, health, kills, deaths, alive });
      }
    } else {
      players = baseline.map((p) => ({ ...p }));
      for (let i = 0; i < pc; i++) {
        const mask = u8(), p = players[i];
        if (mask & M_X) p.x = f32();
        if (mask & M_Y) p.y = f32();
        if (mask & M_Z) p.z = f32();
        if (mask & M_YAW) p.yaw = f32();
        if (mask & M_HEALTH) p.health = u8();
        if (mask & M_KILLS) p.kills = u8();
        if (mask & M_DEATHS) p.deaths = u8();
        if (mask & M_ALIVE) p.alive = u8() === 1;
      }
    }
    baseline = players;
    return { players };
  };
}

function bot(name, aggressive) {
  const wallet = addr();
  const ws = new WebSocket(URL);
  const state = { id: null, seq: 0 };
  const decode = makeSnapshotDecoder();
  // $1 USDC tier — deterministic (no price feed needed for the handshake test)
  ws.on("open", () => ws.send(JSON.stringify({ type: "queue", name, wagerAddress: wallet, wagerTier: 100, wagerToken: "usdc" })));
  ws.on("message", async (raw, isBinary) => {
    if (isBinary) {
      if (raw[0] !== 1 /* BIN_SNAPSHOT */) return;
      const snap = decode(raw);
      if (!aggressive) return;
      const me = snap.players.find((p) => p.id === state.id);
      const foe = snap.players.find((p) => p.id !== state.id);
      if (!me || !foe || !me.alive || !foe.alive) return;
      const dx = foe.x - me.x, dy = foe.y + 1.0 - (me.y + 1.55), dz = foe.z - me.z;
      const far = Math.hypot(dx, dz) > 4;
      const st = state;
      // stuck-escape: if we have not moved while trying to advance, commit to a
      // single lateral direction for 1.5s to slide around the obstacle
      const nowMs = Date.now();
      st.samples = st.samples || [];
      st.samples.push({ t: nowMs, x: me.x, z: me.z });
      st.samples = st.samples.filter((q) => nowMs - q.t < 1200);
      const oldest = st.samples[0];
      const movedSq = (me.x - oldest.x) ** 2 + (me.z - oldest.z) ** 2;
      if (far && nowMs - oldest.t > 900 && movedSq < 0.09 && nowMs > (st.escapeUntil || 0)) {
        st.escapeUntil = nowMs + 1500;
        st.escapeDir = (st.escapeDir || 1) * -1;
        st.samples = [];
      }
      const escaping = nowMs < (st.escapeUntil || 0);

      ws.send(JSON.stringify({
        type: "input",
        input: {
          sequence: state.seq++, forward: far && !escaping, backward: false,
          left: escaping && st.escapeDir < 0, right: escaping && st.escapeDir > 0,
          jump: false, shoot: true,
          yaw: Math.atan2(-dx, -dz), pitch: Math.atan2(dy, Math.hypot(dx, dz)),
        },
      }));
      return;
    }
    // text frames = JSON control messages
    const msg = JSON.parse(String(raw));
    if (msg.type === "ping") { ws.send(JSON.stringify({ type: "pong", t: msg.t })); return; }
    if (msg.type === "welcome") state.id = msg.playerId;
    if (msg.type === "escrow_action") {
      seen[msg.action] = true;
      console.log(`[${name}] escrow_action: ${msg.action} (token=${msg.terms.token.slice(0, 10)}… wager=${msg.terms.wagerWei} usd=${msg.terms.usdCents}c)`);
      // mock mode: no tx to send — the server locks on its next poll
    }
    if (msg.type === "escrow_status" && msg.status === "locked") seen.locked = true;
    if (msg.type === "match_start" && msg.wagered) seen.wageredStart = true;
    if (msg.type === "match_end" && aggressive) {
      const s = msg.settlement;
      console.log(`[${name}] match_end winner=${msg.result.winnerId} settlement=${s ? "present" : "MISSING"}`);
      let sigOk = false;
      if (s) {
        const digest = keccak256(encodePacked(["bytes32", "address"], [s.matchIdHex, s.winnerAddress]));
        const recovered = await recoverMessageAddress({ message: { raw: digest }, signature: s.signature });
        sigOk = recovered.toLowerCase() === String(health.settlementSigner).toLowerCase();
        console.log(`signature recovers to ${recovered} (server signer: ${health.settlementSigner})`);
        console.log(`winner address: ${s.winnerAddress} (mine: ${wallet}) match: ${s.winnerAddress === wallet}`);
      }
      const ok = s && sigOk && s.winnerAddress === wallet &&
        seen.create && seen.join && seen.locked && seen.wageredStart;
      console.log(ok
        ? "\nPASS: wagered flow end-to-end (queue → escrow create/join → locked → duel → signed settlement verifies against MatchEscrow's check)"
        : `\nFAIL: ${JSON.stringify({ ...seen, settlement: !!s, sigOk })}`);
      clearTimeout(deadline);
      process.exit(ok ? 0 : 1);
    }
  });
}

bot("sharpshooter", true);
setTimeout(() => bot("sitting_duck", false), 300);
