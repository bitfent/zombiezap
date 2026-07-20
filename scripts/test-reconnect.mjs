// Reconnect-to-live-match test: one player drops mid-duel, then re-enters within
// the grace window using its match token. The match should PAUSE then RESUME
// (not forfeit). Run with the server up:  node scripts/test-reconnect.mjs
import WebSocket from "ws";

const URL = process.env.SERVER_URL ?? "ws://localhost:8080";
const flags = { dropped: false, reconnected: false, resumed: false, ended: null };
const deadline = setTimeout(() => {
  console.error("FAIL: timed out", flags);
  process.exit(1);
}, 25_000);

function finish() {
  const ok = flags.dropped && flags.reconnected && flags.resumed && !flags.ended;
  console.log(ok ? "\nPASS: drop -> pause -> re-enter -> resume (no forfeit)" : "\nFAIL: " + JSON.stringify(flags));
  clearTimeout(deadline);
  process.exit(ok ? 0 : 1);
}

// Stayer: present throughout; should see opponent_dropped then opponent_reconnected.
function stayer() {
  const ws = new WebSocket(URL);
  ws.on("message", (raw, isBinary) => {
    if (isBinary) return;
    const m = JSON.parse(String(raw));
    if (m.type === "ping") ws.send(JSON.stringify({ type: "pong", t: m.t }));
    if (m.type === "opponent_dropped") { console.log(`[stayer] opponent_dropped (grace ${m.graceMs}ms)`); flags.dropped = true; }
    if (m.type === "opponent_reconnected") { console.log("[stayer] opponent_reconnected"); flags.reconnected = true; if (flags.resumed) finish(); }
    if (m.type === "match_end") { console.log(`[stayer] match_end reason=${m.result.reason}`); flags.ended = m.result.reason; finish(); }
  });
  ws.on("open", () => ws.send(JSON.stringify({ type: "queue", name: "stayer" })));
}

// Leaver: grabs its token, drops the socket, then re-enters 3s later.
function leaver() {
  let token = null;
  const connect = (rejoin) => {
    const ws = new WebSocket(URL);
    ws.on("message", (raw, isBinary) => {
      if (isBinary) return;
      const m = JSON.parse(String(raw));
      if (m.type === "ping") ws.send(JSON.stringify({ type: "pong", t: m.t }));
      if (m.type === "welcome" && rejoin) ws.send(JSON.stringify({ type: "rejoin_match", token }));
      if (m.type === "match_token") {
        token = m.token;
        // drop AFTER the 3s countdown so we're mid-duel (IN_PROGRESS), where the
        // grace/pause applies (a drop during the countdown just forfeits)
        if (!rejoin) { console.log("[leaver] got token — dropping mid-duel in 4s"); setTimeout(() => ws.close(), 4000); }
      }
      if (m.type === "match_resumed") { console.log("[leaver] match_resumed — back in"); flags.resumed = true; if (flags.reconnected) finish(); }
    });
    if (!rejoin) {
      ws.on("open", () => ws.send(JSON.stringify({ type: "queue", name: "leaver" })));
      ws.on("close", () => { console.log("[leaver] dropped — reconnecting in 3s"); setTimeout(() => connect(true), 3000); });
    }
  };
  connect(false);
}

stayer();
setTimeout(leaver, 300);
