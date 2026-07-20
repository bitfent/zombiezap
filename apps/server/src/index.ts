// ShotAnte game server: WebSocket in, authoritative simulation inside,
// snapshots out. HTTP surface: WS upgrade, /health, and the small sharing
// API — /api/upcoming + /api/challenge/:code (JSON for the board/landing
// card) and /c/:code (per-challenge Open Graph unfurl that redirects humans
// to the web app).

import "./env.ts"; // MUST be first — loads .env before any module reads process.env
import { createServer, type ServerResponse } from "node:http";
import { randomUUID } from "node:crypto";
import { WebSocketServer, type WebSocket } from "ws";
import {
  CHAT_MAX_LEN,
  CHAT_RATE_MS,
  NATIVE,
  PING_INTERVAL_MS,
  PRESENCE_TIMEOUT_MS,
  PROTOCOL_VERSION,
  VOICE_MAX_FRAME_BYTES,
  parseClientMessage,
  type ChallengeInfo,
} from "@shotante/shared";
import { ServerPlayer } from "./game/Player.ts";
import { DuelQueue } from "./matchmaking/queue.ts";
import { escrowFeeBps, initEscrowFee, relayerAddress, settlementAddress, wagerMode } from "./settlement/escrow.ts";
import { dbStatus, initDb, loadOpenChallenges } from "./db.ts";
import { readReplay } from "./replay.ts";

const EVM_ADDR = /^0x[0-9a-fA-F]{40}$/;
const CODE_RE = /^[A-Za-z0-9]{5}$/;

const PORT = Number(process.env.PORT ?? 8080);
// Where /c/:code sends humans (the web app). Production: https://shotante.com
const WEB_URL = (process.env.WEB_URL ?? "https://shotante.com").replace(/\/$/, "");

// ERC-7677 paymaster proxy: the CDP (or other) paymaster RPC URL carries a
// secret key, so the browser never sees it — it calls our /api/paymaster, we
// forward the gas-sponsorship RPC. Unset = sponsorship simply off.
const PAYMASTER_RPC_URL = process.env.PAYMASTER_RPC_URL ?? null;
// Only these ERC-7677 methods are relayed; anything else is rejected so the
// proxy can't be used as an open relay to the upstream RPC.
const PAYMASTER_METHODS = new Set(["pm_getPaymasterStubData", "pm_getPaymasterData"]);

function readBody(req: import("node:http").IncomingMessage, limitBytes = 16_384): Promise<string> {
  return new Promise((resolve, reject) => {
    let data = "";
    req.on("data", (chunk) => {
      data += chunk;
      if (data.length > limitBytes) reject(new Error("body too large"));
    });
    req.on("end", () => resolve(data));
    req.on("error", reject);
  });
}

async function handlePaymaster(req: import("node:http").IncomingMessage, res: ServerResponse): Promise<void> {
  if (!PAYMASTER_RPC_URL) {
    sendJson(res, 503, { error: "gas sponsorship not configured" });
    return;
  }
  let body: any;
  try {
    body = JSON.parse(await readBody(req));
  } catch {
    sendJson(res, 400, { error: "invalid JSON" });
    return;
  }
  if (!body || typeof body.method !== "string" || !PAYMASTER_METHODS.has(body.method)) {
    sendJson(res, 403, { error: "method not allowed" });
    return;
  }
  try {
    const upstream = await fetch(PAYMASTER_RPC_URL, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    const text = await upstream.text();
    res.writeHead(upstream.status, {
      "content-type": "application/json",
      "access-control-allow-origin": "*",
    });
    res.end(text);
  } catch {
    sendJson(res, 502, { error: "paymaster upstream unreachable" });
  }
}

const queue = new DuelQueue();

function sendJson(res: ServerResponse, status: number, body: unknown): void {
  res.writeHead(status, {
    "content-type": "application/json",
    "access-control-allow-origin": "*", // public read-only data; the SPA is on another origin
  });
  res.end(JSON.stringify(body));
}

const escapeHtml = (s: string) =>
  s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);

/** Human pot label: ETH antes are 18 decimals, USDC (any ERC-20 here) is 6. */
function potLabel(info: ChallengeInfo): string | null {
  if (info.potWei === null) return null;
  const eth = info.token === null || info.token === NATIVE;
  const amount = Number(info.potWei) / (eth ? 1e18 : 1e6);
  return `${amount.toLocaleString("en-US", { maximumFractionDigits: 6 })} ${eth ? "ETH" : "USDC"}`;
}

/** Per-challenge unfurl: the link IS the challenge card on social/chat apps. */
function challengePage(info: ChallengeInfo): string {
  const host = escapeHtml(info.hostName);
  const pot = info.potWei !== null ? `${potLabel(info)} pot on the line` : "free duel";
  const when =
    info.scheduledAt !== null
      ? new Date(info.scheduledAt).toUTCString().replace(":00 GMT", " UTC")
      : "right now";
  const title = `${host} challenges you — ShotAnte`;
  const desc = `Retro 1v1 duel, ${pot}. Match: ${when}. No install — click to answer the challenge.`;
  const url = `${WEB_URL}/?join=${info.code}`;
  return `<!doctype html><html lang="en"><head><meta charset="utf-8">
<title>${title}</title>
<meta property="og:type" content="website">
<meta property="og:site_name" content="ShotAnte">
<meta property="og:title" content="${title}">
<meta property="og:description" content="${desc}">
<meta property="og:url" content="${url}">
<meta property="og:image" content="${WEB_URL}/og.jpg">
<meta property="og:image:type" content="image/jpeg">
<meta property="og:image:width" content="1200">
<meta property="og:image:height" content="630">
<meta name="twitter:card" content="summary_large_image">
<meta name="twitter:title" content="${title}">
<meta name="twitter:description" content="${desc}">
<meta name="twitter:image" content="${WEB_URL}/og.jpg">
<meta http-equiv="refresh" content="0;url=${url}">
</head><body><p>Answering the challenge… <a href="${url}">continue</a></p>
<script>location.replace(${JSON.stringify(url)})</script></body></html>`;
}

const http = createServer((req, res) => {
  const url = new URL(req.url ?? "/", "http://localhost");
  if (url.pathname === "/api/paymaster") {
    if (req.method === "OPTIONS") {
      res.writeHead(204, {
        "access-control-allow-origin": "*",
        "access-control-allow-methods": "POST, OPTIONS",
        "access-control-allow-headers": "content-type",
        "access-control-max-age": "86400",
      });
      res.end();
      return;
    }
    if (req.method === "POST") {
      void handlePaymaster(req, res);
      return;
    }
    sendJson(res, 405, { error: "method not allowed" });
    return;
  }
  if (url.pathname === "/health") {
    sendJson(res, 200, {
      status: "ok",
      protocol: PROTOCOL_VERSION,
      wagerMode,
      feeBps: escrowFeeBps(), // protocol fee on wagered pots (0 when wagers are off)
      db: dbStatus(),
      settlementSigner: wagerMode === "off" ? null : settlementAddress,
      relayer: relayerAddress, // gas wallet that auto-pays winners via settle() (null = client claims)
      gasSponsorship: !!PAYMASTER_RPC_URL, // gas-sponsored ante/claim available (legacy/EOA path)
    });
    return;
  }
  if (url.pathname === "/api/upcoming") {
    sendJson(res, 200, queue.upcoming());
    return;
  }
  const apiMatch = url.pathname.match(/^\/api\/challenge\/([A-Za-z0-9]{5})$/);
  if (apiMatch) {
    const info = queue.challengeInfo(apiMatch[1]);
    if (info) sendJson(res, 200, info);
    else sendJson(res, 404, { error: "challenge not found" });
    return;
  }
  const replayMatch = url.pathname.match(/^\/api\/replay\/([A-Za-z0-9-]{1,40})$/);
  if (replayMatch) {
    void readReplay(replayMatch[1]).then((json) => {
      if (json) {
        res.writeHead(200, { "content-type": "application/json", "access-control-allow-origin": "*" });
        res.end(json);
      } else {
        sendJson(res, 404, { error: "replay not found" });
      }
    });
    return;
  }
  const cMatch = url.pathname.match(/^\/c\/([A-Za-z0-9]{5})$/);
  if (cMatch) {
    const info = queue.challengeInfo(cMatch[1]);
    res.writeHead(info ? 200 : 404, { "content-type": "text/html; charset=utf-8" });
    if (info) res.end(challengePage(info));
    else res.end(`<!doctype html><meta http-equiv="refresh" content="0;url=${WEB_URL}"><p>Challenge over — <a href="${WEB_URL}">play ShotAnte</a></p>`);
    return;
  }
  res.writeHead(404);
  res.end();
});

const wss = new WebSocketServer({ server: http });

wss.on("connection", (ws: WebSocket) => {
  let playerId = randomUUID().slice(0, 8); // reassigned on a successful match rejoin
  let player: ServerPlayer | null = null;

  ws.send(JSON.stringify({ type: "welcome", playerId, protocol: PROTOCOL_VERSION }));

  // Heartbeat + RTT probe: ping periodically; the client echoes a pong (RTT). ANY
  // message counts as presence. If a client goes silent (closed tab / backgrounded /
  // dropped) we terminate the socket — which routes into queue.disconnect ->
  // match forfeit, so the player who STAYED wins promptly instead of the match
  // hanging until the minutes-long TCP timeout.
  let lastSeenAt = Date.now();
  const pingTimer = setInterval(() => {
    if (ws.readyState !== ws.OPEN) return;
    if (Date.now() - lastSeenAt > PRESENCE_TIMEOUT_MS) {
      ws.terminate(); // -> "close" -> queue.disconnect -> forfeit to the player who stayed
      return;
    }
    ws.send(JSON.stringify({ type: "ping", t: Date.now() }));
  }, PING_INTERVAL_MS);

  ws.on("message", (raw, isBinary) => {
    lastSeenAt = Date.now(); // any traffic = the client is present
    // Binary frames are proximity VOICE audio (raw PCM). Relay to the opponent
    // when in a match; the server enforces the proximity gate authoritatively.
    if (isBinary) {
      if (!player) return;
      const frame = raw as Buffer;
      if (frame.length === 0 || frame.length > VOICE_MAX_FRAME_BYTES) return;
      queue.matchOf(playerId)?.relayVoice(playerId, frame);
      return;
    }

    const msg = parseClientMessage(String(raw));
    if (!msg) return;

    // Messages that carry a player identity (name + optional wager wallet + stake).
    const withPlayer = (
      name: string,
      wagerAddress?: string,
      wagerTier?: number,
      wagerToken?: "usdc" | "eth",
    ): ServerPlayer | null => {
      if (!player) {
        player = new ServerPlayer(
          playerId,
          name,
          (json) => {
            if (ws.readyState === ws.OPEN) ws.send(json);
          },
          (data) => {
            if (ws.readyState === ws.OPEN) ws.send(data, { binary: true });
          },
        );
      } else {
        player.rename(name); // e.g. board-browser ("browser") now joining with a callsign
      }
      if (wagerAddress !== undefined) {
        if (wagerMode === "off") {
          ws.send(JSON.stringify({ type: "error", message: "wagered duels are not enabled on this server" }));
          return null;
        }
        if (!EVM_ADDR.test(wagerAddress)) {
          ws.send(JSON.stringify({ type: "error", message: "invalid wallet address" }));
          return null;
        }
        player.wagerAddress = wagerAddress.toLowerCase();
        player.wagerTier = typeof wagerTier === "number" ? wagerTier : null;
        player.wagerTokenKind = wagerToken === "usdc" || wagerToken === "eth" ? wagerToken : null;
      } else {
        player.wagerAddress = null;
        player.wagerTier = null;
        player.wagerTokenKind = null;
      }
      return player;
    };

    // A wallet on a free join is harmless (the challenge decides); only treat
    // it as a wager request when wagers are enabled at all.
    const joinAddr = (addr?: string) => (wagerMode === "off" ? undefined : addr);

    switch (msg.type) {
      case "queue": {
        const p = withPlayer(msg.name, msg.wagerAddress, msg.wagerTier, msg.wagerToken);
        if (p) queue.enqueue(p);
        return;
      }
      case "create_challenge": {
        const p = withPlayer(msg.name, msg.wagerAddress, msg.wagerTier, msg.wagerToken);
        if (p)
          queue.createChallenge(p, {
            isPublic: msg.isPublic,
            scheduledAt: msg.scheduledAt,
            reminderEmail: msg.reminderEmail,
          });
        return;
      }
      case "join_challenge":
      case "reserve_challenge": {
        // join and reserve are the same server move: fill the opponent slot;
        // play-now starts immediately, scheduled waits for its window.
        if (!CODE_RE.test(msg.code)) return;
        const p = withPlayer(msg.name, joinAddr(msg.wagerAddress));
        if (p) queue.joinChallenge(p, msg.code);
        return;
      }
      case "reclaim_challenge": {
        if (!CODE_RE.test(msg.code)) return;
        const p = withPlayer(msg.name, joinAddr(msg.wagerAddress));
        if (p) void queue.reclaimChallenge(p, msg.code, msg.key, msg.signature);
        return;
      }
      case "reclaim_nonce_request": {
        if (!CODE_RE.test(msg.code)) return;
        const addr = joinAddr(msg.wagerAddress);
        const p = withPlayer(player?.name ?? "browser", addr);
        if (p && addr) queue.requestReclaimNonce(p, msg.code, addr);
        return;
      }
      case "lobby_ready":
        queue.lobbyReady(playerId, msg.ready === true);
        return;
      case "leave_lobby":
        queue.leaveLobby(playerId);
        return;
      case "cancel_challenge":
        if (CODE_RE.test(msg.code)) queue.cancelChallenge(playerId, msg.code, msg.hostKey);
        return;
      case "sub_board": {
        const p = withPlayer(player?.name ?? "browser");
        if (p) queue.subBoard(p);
        return;
      }
      case "unsub_board":
        queue.unsubBoard(playerId);
        return;
      case "chat": {
        // Lobby chat only — ephemeral, sanitized, rate-limited, never stored.
        // In-match proximity chat is now VOICE (binary frames above), so we drop
        // text once a match is underway.
        if (!player || typeof msg.text !== "string") return;
        if (queue.matchOf(playerId)) return; // no in-match text chat anymore
        const now = Date.now();
        if (now - player.lastChatAt < CHAT_RATE_MS) return;
        // eslint-disable-next-line no-control-regex
        const text = msg.text.replace(/[\u0000-\u001f\u007f]/g, "").trim().slice(0, CHAT_MAX_LEN);
        if (!text) return;
        player.lastChatAt = now;
        queue.lobbyChat(playerId, player.name, text);
        return;
      }
      case "leave_queue":
        queue.leave(playerId);
        return;
      case "input":
        if (player) player.acceptInput(msg.input, Date.now());
        return;
      case "pong": {
        // RTT = now - the timestamp we put in the ping; smooth it (EMA)
        if (player && typeof msg.t === "number") {
          const rtt = Date.now() - msg.t;
          if (rtt >= 0 && rtt < 5000) player.rttMs = player.rttMs * 0.7 + rtt * 0.3;
        }
        return;
      }
      case "rejoin_match": {
        // Re-enter a live match after a drop: rebind the existing player to THIS
        // socket and adopt its id, so inputs/voice/disconnect all route correctly.
        const rejoined = queue.rejoinMatch(
          msg.token,
          (json) => {
            if (ws.readyState === ws.OPEN) ws.send(json);
          },
          (data) => {
            if (ws.readyState === ws.OPEN) ws.send(data, { binary: true });
          },
        );
        if (rejoined) {
          player = rejoined;
          playerId = rejoined.id;
        } else {
          ws.send(JSON.stringify({ type: "error", message: "could not rejoin — match ended" }));
        }
        return;
      }
    }
  });

  ws.on("close", () => {
    clearInterval(pingTimer);
    queue.disconnect(playerId);
  });
  ws.on("error", () => {
    clearInterval(pingTimer);
    queue.disconnect(playerId);
  });
});

// Boot: connect the durable layer, then restore open challenges so invite
// links and scheduled matches survive a server restart.
void initDb().then(async () => {
  queue.restore(await loadOpenChallenges());
});
void initEscrowFee(); // chain mode: pick up the on-chain feeBps

http.listen(PORT, () => {
  console.log(`shotante server listening on :${PORT} (ws + /health + /api + /c)`);
  console.log(
    `wager mode: ${wagerMode}` +
      (wagerMode !== "off" ? ` · settlement signer: ${settlementAddress}` : "") +
      (relayerAddress ? ` · relayer (auto-payout): ${relayerAddress}` : ""),
  );
});
