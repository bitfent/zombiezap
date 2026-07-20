// Headless verification of the CHALLENGE system (persistent DeathMatch lobbies):
//
// Scenario A — play now, persistence:
//   host creates a public challenge -> HTTP card api works -> host DISCONNECTS
//   (challenge must survive) -> host reclaims with hostKey -> guest finds it
//   on the live board and joins -> match starts instantly -> duel runs to a
//   hashed result with the host winning. (In-match comms are now proximity
//   VOICE — binary audio — which this text-based bot doesn't exercise.)
//
// Scenario B — minimize (prompt-to-start), leave_lobby, lobby chat:
//   host creates a play-now challenge then steps away (lobby_ready:false, as
//   the BACK button does) -> guest joins and auto-readies -> the match does
//   NOT start while the host is away -> lobby chat flows both ways -> the host
//   taps START (lobby_ready:true) -> the match fires -> a disconnect forfeits.
//
// Run the server first (npm run dev:server), then: npm run verify:challenge

import WebSocket from "ws";

const URL = process.env.SERVER_URL ?? "ws://localhost:8080";
const HTTP = URL.replace(/^ws/, "http");
const deadline = setTimeout(() => {
  console.error("FAIL: challenge flows did not complete within 200s");
  process.exit(1);
}, 200_000);

function fail(why) {
  console.error(`FAIL: ${why}`);
  process.exit(1);
}

// closeCombat: hold fire until point-blank (forces the host to physically close
// the distance). Kept optional; the duel resolves either way vs a passive guest.
function aimbot(ws, state, snap, closeCombat = false) {
  const me = snap.players.find((p) => p.id === state.id);
  const foe = snap.players.find((p) => p.id !== state.id);
  if (!me || !foe || !me.alive || !foe.alive) return;
  const dx = foe.x - me.x, dy = foe.y + 1.0 - (me.y + 1.55), dz = foe.z - me.z;
  const dist = Math.hypot(dx, dz);
  const far = dist > 4;
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
    // every 3rd failed attempt: REVERSE out of dead-end pockets (two-room
    // interiors, bridge underpasses) on the 60x60 maps
    st.escapeCount = (st.escapeCount || 0) + 1;
    st.escapeBack = st.escapeCount % 3 === 0;
    st.samples = [];
  }
  const escaping = nowMs < (st.escapeUntil || 0);

  ws.send(JSON.stringify({
    type: "input",
    input: {
      sequence: state.seq++,
      // escaping: diagonal slide hugs corners; hop the climbable crates
      forward: far && !(escaping && st.escapeBack),
      backward: escaping && st.escapeBack,
      left: escaping && st.escapeDir < 0, right: escaping && st.escapeDir > 0,
      jump: escaping, shoot: !closeCombat || dist < 10,
      yaw: Math.atan2(-dx, -dz), pitch: Math.atan2(dy, Math.hypot(dx, dz)),
    },
  }));
}

// ════════════════════════════════ Scenario A ════════════════════════════════
const A = {
  created: false, httpCard: false, reclaimed: false, listed: false,
  started: false, done: false,
};

function scenarioA() {
  console.log("— scenario A: play now · persistence —");
  const host1 = new WebSocket(URL);
  let code = null, hostKey = null;

  host1.on("open", () =>
    host1.send(JSON.stringify({ type: "create_challenge", name: "host_bot", isPublic: true })));
  host1.on("message", async (raw) => {
    const msg = JSON.parse(String(raw));
    if (msg.type === "challenge_created") {
      code = msg.code;
      hostKey = msg.hostKey;
      A.created = true;
      console.log(`[hostA] challenge created: ${code}`);

      // HTTP surface: landing-card JSON + OG unfurl page
      const api = await fetch(`${HTTP}/api/challenge/${code}`);
      const info = api.ok ? await api.json() : null;
      const og = await fetch(`${HTTP}/c/${code}`);
      const ogHtml = og.ok ? await og.text() : "";
      A.httpCard = !!info && info.hostName === "host_bot" && info.status === "open" &&
        ogHtml.includes("og:title") && ogHtml.includes("host_bot");
      if (!A.httpCard) fail("HTTP challenge card or /c/:code unfurl broken");

      // The point of persistence: the host's tab "closes"…
      host1.close();
      setTimeout(() => reclaimA(code, hostKey), 400);
    }
  });
}

function reclaimA(code, hostKey) {
  const host2 = new WebSocket(URL);
  const state = { id: null, seq: 0 };

  host2.on("open", () =>
    host2.send(JSON.stringify({ type: "reclaim_challenge", code, key: hostKey, name: "host_bot" })));
  host2.on("message", (raw) => {
    const msg = JSON.parse(String(raw));
    if (msg.type === "welcome") state.id = msg.playerId;
    if (msg.type === "error") fail(`hostA reclaim: ${msg.message}`);
    if (msg.type === "lobby_state" && !A.reclaimed) {
      if (!msg.lobby.host.present) fail("reclaim did not re-attach the host");
      A.reclaimed = true;
      console.log(`[hostA] reclaimed ${code} after disconnect — spawning guest`);
      spawnGuestA(code);
    }
    if (msg.type === "match_start") {
      // the guest browsed the board first — their callsign must still win out
      if (!msg.players.some((p) => p.name === "guest_bot")) fail("joiner kept the placeholder name");
      A.started = true;
      console.log(`[hostA] match started instantly on join`);
    }
    if (msg.type === "snapshot") {
      aimbot(host2, state, msg.snapshot, true);
    }
    if (msg.type === "match_end") {
      const r = msg.result;
      if (r.winnerId !== state.id) fail(`hostA expected to win, got ${r.winnerId} (${r.reason})`);
      if (!/^[0-9a-f]{64}$/.test(r.resultHash)) fail("bad result hash");
      A.done = true;
      console.log(`[hostA] match_end reason=${r.reason} hash ok`);
      setTimeout(scenarioB, 300);
    }
  });
}

function spawnGuestA(code) {
  const ws = new WebSocket(URL);
  ws.on("open", () => ws.send(JSON.stringify({ type: "sub_board" })));
  ws.on("message", (raw) => {
    const msg = JSON.parse(String(raw));
    if (msg.type === "board_update" && !A.listed) {
      const found = msg.upcoming.find((c) => c.code === code);
      if (!found) fail("public challenge missing from the live board");
      A.listed = true;
      console.log(`[guestA] saw ${code} on the board (status=${found.status}) — joining`);
      ws.send(JSON.stringify({ type: "join_challenge", code, name: "guest_bot" }));
    }
    if (msg.type === "error") fail(`guestA: ${msg.message}`);
    // guest plays passively (sitting duck) so the host wins deterministically
  });
}

// ════════════════════════════════ Scenario B ════════════════════════════════
const B = {
  joined: false, hostGotChat: false, guestGotChat: false, expiresAt: false,
  heldWhileAway: true, started: false, done: false,
};

function scenarioB() {
  console.log("— scenario B: minimize (prompt-to-start) · leave_lobby · lobby chat —");
  const host = new WebSocket(URL);
  const guest = new WebSocket(URL);
  let code = null;
  let guestPresentAt = 0;

  host.on("open", () =>
    host.send(JSON.stringify({ type: "create_challenge", name: "away_host", isPublic: true })));
  host.on("message", (raw) => {
    const msg = JSON.parse(String(raw));
    if (msg.type === "challenge_created") {
      code = msg.code;
      // expiry must be surfaced (Home card countdown depends on it)
      if (typeof msg.info.expiresAt !== "number" || msg.info.expiresAt <= Date.now()) {
        fail("challenge_created info missing a future expiresAt");
      }
      B.expiresAt = true;
      console.log(`[hostB] created ${code} — stepping away (lobby_ready:false)`);
      // BACK / minimize: stay attached but not ready, so a joiner is HELD
      host.send(JSON.stringify({ type: "lobby_ready", ready: false }));
      setTimeout(() => guest.send(JSON.stringify({ type: "join_challenge", code, name: "back_guest" })), 300);
    }
    if (msg.type === "lobby_state" && msg.lobby.guest?.present && !B.joined) {
      B.joined = true;
      guestPresentAt = Date.now();
      if (typeof msg.lobby.expiresAt !== "number") fail("lobby_state missing expiresAt");
      console.log(`[hostB] guest is here while host is away — chatting, then START in ~1.5s`);
      host.send(JSON.stringify({ type: "chat", text: "brb — tap when ready" }));
      // Give the server a beat: if it (wrongly) started, match_start lands first.
      setTimeout(() => {
        console.log(`[hostB] tapping START (lobby_ready:true)`);
        host.send(JSON.stringify({ type: "lobby_ready", ready: true }));
      }, 1500);
    }
    if (msg.type === "chat" && msg.scope === "lobby" && msg.text === "on my way") {
      B.hostGotChat = true;
    }
    if (msg.type === "match_start") {
      // must only fire AFTER the host taps START, never while away
      if (guestPresentAt && Date.now() - guestPresentAt < 1400) B.heldWhileAway = false;
      B.started = true;
      console.log(`[hostB] match started after START tap ✓`);
      setTimeout(() => guest.close(), 1500); // forfeit: scenario B is about the lobby
    }
    if (msg.type === "match_end") {
      if (msg.result.reason !== "forfeit") fail(`expected forfeit, got ${msg.result.reason}`);
      B.done = true;
      finish();
    }
    if (msg.type === "error") fail(`hostB: ${msg.message}`);
  });

  guest.on("message", (raw) => {
    const msg = JSON.parse(String(raw));
    if (msg.type === "chat" && msg.scope === "lobby" && msg.text === "brb — tap when ready") {
      B.guestGotChat = true;
      guest.send(JSON.stringify({ type: "chat", text: "on my way" }));
    }
    if (msg.type === "error") fail(`guestB: ${msg.message}`);
  });
}

scenarioA();

function finish() {
  const okA = A.created && A.httpCard && A.reclaimed && A.listed && A.started &&
    A.done;
  const okB = B.joined && B.expiresAt && B.hostGotChat && B.guestGotChat &&
    B.heldWhileAway && B.started && B.done;
  console.log(okA && okB
    ? "\nPASS: DeathMatch end-to-end (create → survive disconnect → reclaim → board → join → result; minimize holds start → lobby chat → START fires → forfeit)"
    : `\nFAIL: A=${JSON.stringify(A)} B=${JSON.stringify(B)}`);
  clearTimeout(deadline);
  process.exit(okA && okB ? 0 : 1);
}
