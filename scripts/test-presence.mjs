// Heartbeat / room-presence test: one player goes SILENT mid-match (socket stays
// open, just stops responding — simulates a closed tab / backgrounded phone that
// didn't send a clean close). The server's heartbeat should drop it within
// PRESENCE_TIMEOUT and forfeit the match to the player who STAYED.
//   node scripts/test-presence.mjs   (server must be running)
import WebSocket from "ws";

const URL = process.env.SERVER_URL ?? "ws://localhost:8080";
const t0 = Date.now();
// A silent player is detected by the heartbeat (~8s) then gets the reconnect
// grace (~20s) before forfeiting — so allow ~35s total.
const deadline = setTimeout(() => {
  console.error("FAIL: no forfeit — match kept running after a player left");
  process.exit(1);
}, 35_000);

// Stays present (pongs), waits to be declared the winner by forfeit.
function stayer() {
  const ws = new WebSocket(URL);
  let id = null;
  let startedAt = 0;
  ws.on("message", (raw, isBinary) => {
    if (isBinary) return;
    const m = JSON.parse(String(raw));
    if (m.type === "welcome") id = m.playerId;
    if (m.type === "ping") ws.send(JSON.stringify({ type: "pong", t: m.t }));
    if (m.type === "match_start") {
      startedAt = Date.now();
      console.log("[stayer] match started — staying present");
    }
    if (m.type === "opponent_dropped") console.log(`[stayer] opponent_dropped — ${m.graceMs}ms to reconnect`);
    if (m.type === "match_end") {
      const r = m.result;
      const since = ((Date.now() - startedAt) / 1000).toFixed(1);
      console.log(`[stayer] match_end reason=${r.reason} winner=${r.winnerId} (me=${id}) ${since}s after start`);
      const ok = r.reason === "forfeit" && r.winnerId === id;
      console.log(ok ? "\nPASS: a player who left was dropped and the stayer won by forfeit" : "\nFAIL: unexpected end");
      clearTimeout(deadline);
      process.exit(ok ? 0 : 1);
    }
  });
  ws.on("open", () => ws.send(JSON.stringify({ type: "queue", name: "stayer" })));
}

// Queues, then on match_start goes fully SILENT (keeps the socket open).
function leaver() {
  const ws = new WebSocket(URL);
  ws.on("message", (raw, isBinary) => {
    if (isBinary) return;
    const m = JSON.parse(String(raw));
    if (m.type === "ping") ws.send(JSON.stringify({ type: "pong", t: m.t }));
    if (m.type === "match_start") {
      console.log("[leaver] match started — going SILENT (socket stays open)");
      ws.removeAllListeners("message"); // stop ponging / all responses
    }
  });
  ws.on("open", () => ws.send(JSON.stringify({ type: "queue", name: "leaver" })));
}

stayer();
setTimeout(leaver, 300);
