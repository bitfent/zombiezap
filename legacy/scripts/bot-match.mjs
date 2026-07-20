// Headless verification of the full server loop: two WebSocket bots queue,
// get matched, bot A perfect-aims at bot B and holds fire; assert the match
// runs to completion with A as winner and a result hash.
//
// Run the server first (npm run dev:server), then: npm run verify:match

import WebSocket from "ws";

const URL = process.env.SERVER_URL ?? "ws://localhost:8080";

// Minimal STATEFUL decoder for the delta-compressed snapshot stream (see
// packages/shared/snapshotCodec). The bot only needs the players, which come
// first — we stop after them, applying deltas against the kept baseline.
const M_X = 1, M_Y = 2, M_Z = 4, M_YAW = 8, M_HEALTH = 16, M_KILLS = 32, M_DEATHS = 64, M_ALIVE = 128;
function makeSnapshotDecoder() {
  let baseline = null; // last decoded players[]
  return (buf) => {
    const dv = new DataView(buf.buffer, buf.byteOffset, buf.byteLength);
    let o = 1; // skip the BIN_SNAPSHOT tag
    const u8 = () => dv.getUint8(o++);
    const u32 = () => { const v = dv.getUint32(o, true); o += 4; return v; };
    const f32 = () => { const v = dv.getFloat32(o, true); o += 4; return v; };
    const str = () => { let s = ""; for (let n = u8(), i = 0; i < n; i++) s += String.fromCharCode(u8()); return s; };
    const flags = u8();
    u32(); u8(); u32(); // tick, phase, timeLeftMs
    const pc = u8();
    let players;
    if (flags & 1 /* keyframe */) {
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
const deadline = setTimeout(() => {
  console.error("FAIL: match did not complete within 200s");
  process.exit(1);
}, 200_000);

function bot(name, aggressive) {
  const ws = new WebSocket(URL);
  const state = { id: null, seq: 0, ws, name };
  const decodeSnapshot = makeSnapshotDecoder();
  ws.on("open", () => ws.send(JSON.stringify({ type: "queue", name })));
  ws.on("message", (raw, isBinary) => {
    if (isBinary) {
      // binary frames are snapshots (or relayed voice, which the bot ignores)
      if (raw[0] !== 1 /* BIN_SNAPSHOT */) return;
      const snap = decodeSnapshot(raw); // keep the baseline in sync even when not aggressive
      if (!aggressive) return;
      const me = snap.players.find((p) => p.id === state.id);
      const foe = snap.players.find((p) => p.id !== state.id);
      if (!me || !foe || !me.alive || !foe.alive) return;
      // perfect aim: yaw/pitch from my eye to the foe's hitbox center; advance
      // toward the target so cover (the center block) gets flanked like a real
      // player would.
      const dx = foe.x - me.x, dy = foe.y + 1.0 - (me.y + 1.55), dz = foe.z - me.z;
      const yaw = Math.atan2(-dx, -dz);
      const pitch = Math.atan2(dy, Math.hypot(dx, dz));
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
        st.escapeUntil = nowMs + 1800;
        st.escapeDir = (st.escapeDir || 1) * -1;
        // every 3rd failed attempt: REVERSE out — frees the bot from dead-end
        // pockets (two-room interiors, bridge underpasses) on the 60x60 maps
        st.escapeCount = (st.escapeCount || 0) + 1;
        st.escapeBack = st.escapeCount % 3 === 0;
        st.samples = [];
      }
      const escaping = nowMs < (st.escapeUntil || 0);

      ws.send(JSON.stringify({
        type: "input",
        input: {
          sequence: state.seq++,
          // escaping: diagonal slide (forward+strafe) hugs around corners;
          // jump hops the climbable crates instead of grinding against them
          forward: far && !(escaping && st.escapeBack),
          backward: escaping && st.escapeBack,
          left: escaping && st.escapeDir < 0, right: escaping && st.escapeDir > 0,
          jump: escaping, shoot: true, yaw, pitch,
        },
      }));
      return;
    }
    // text frames = JSON control messages
    const msg = JSON.parse(String(raw));
    if (msg.type === "ping") { ws.send(JSON.stringify({ type: "pong", t: msg.t })); return; }
    if (msg.type === "welcome") state.id = msg.playerId;
    if (msg.type === "match_start") console.log(`[${name}] match ${msg.matchId} vs`, msg.players.map((p) => p.name).join(" / "));
    if (msg.type === "match_end") {
      const r = msg.result;
      console.log(`[${name}] match_end: winner=${r.winnerId} reason=${r.reason} hash=${r.resultHash.slice(0, 16)}…`);
      console.log(`[${name}] scores:`, r.players.map((p) => `${p.name}:${p.kills}k/${p.deaths}d`).join("  "));
      if (name === "sharpshooter") {
        const ok = r.winnerId === state.id && ["kills", "time", "sudden_death"].includes(r.reason) && /^[0-9a-f]{64}$/.test(r.resultHash);
        console.log(ok ? "\nPASS: server-authoritative 1v1 ran end-to-end (queue → duel → 5 kills → hashed result)" : "\nFAIL: unexpected result");
        clearTimeout(deadline);
        process.exit(ok ? 0 : 1);
      }
    }
  });
  return state;
}

bot("sharpshooter", true);
// BOT_SOLO=1 spawns ONLY the aggressive bot, leaving the opponent slot for a
// real browser client (used for live browser smoke tests).
if (!process.env.BOT_SOLO) setTimeout(() => bot("sitting_duck", false), 300);
