// Entry: menu + challenge/lobby wiring. The flow the UX is built around:
//   connect wallet once (optional) → PRACTICE / QUICK 1v1 instantly, or
//   CREATE CHALLENGE (public/private, free/wagered, now/scheduled) → share the
//   invite link / QR → friend lands on shotante.com/?join=CODE, sees the
//   challenge card and joins in one tap — or UPCOMING DUELS to join/reserve a
//   public one. Challenges survive refreshes: host/guest keys live in
//   localStorage and re-enter the lobby via reclaim_challenge.

import "./style.css";
import QRCode from "qrcode";
import {
  NATIVE,
  RECONNECT_GRACE_MS,
  type ChallengeInfo,
  type LobbyState,
  type PlayerInput,
  type ServerMessage,
  type WagerTerms,
} from "@shotante/shared";
import { Game } from "./game/Game.ts";
import { isTouchDevice } from "./game/Input.ts";
import { GameSocket } from "./network/socket.ts";
import { connectWallet, connectInjected, listInjectedWallets, shortAddress } from "./wallet/connect.ts";
import type { InjectedWallet } from "./wallet/provider.ts";
import type { Address, Hex } from "viem";

// viem (+ the escrow module) is the heaviest dependency after three.js, and
// only wagered games need it — load it on demand so the game itself stays
// light. Vite splits this into its own chunk automatically.
const escrow = () => import("./wallet/escrow.ts");

const TOUCH = isTouchDevice();
if (TOUCH) document.body.classList.add("touch");

const env = (import.meta as any).env ?? {};
const WS_URL: string = env.VITE_SERVER_URL ?? "ws://localhost:8080";
const API_URL = WS_URL.replace(/^ws/, "http");
// Share links: with VITE_SHARE_BASE set (a host that rewrites /c/:code to the
// game server) links unfurl as per-challenge cards; without it we fall back to
// the plain SPA link, which always works.
const SHARE_BASE: string | null = env.VITE_SHARE_BASE ?? null;

const $ = (id: string) => document.getElementById(id)!;
const canvas = $("game") as HTMLCanvasElement;
const menu = $("menu");
const status = $("menu-status");
const nameInput = $("name") as HTMLInputElement;
const claimBtn = $("claim-btn") as HTMLButtonElement;
const rematchBtn = $("rematch-btn") as HTMLButtonElement;

const game = new Game(canvas);
// dev-only handle for inspecting/iterating on the renderer (avatars, animation,
// camera) from the console — stripped from production builds
if (env.DEV) (window as any).__game = game;
const socket = new GameSocket();
if (env.DEV) (window as any).__socket = socket; // dev: force a drop to test reconnect
// Proximity VOICE: outgoing mic frames go out as binary; incoming binary is the
// opponent's voice. The server gates both ends by the 20m proximity radius.
game.bindVoiceOut((buf) => socket.sendBinary(buf));
socket.onBinary = (buf) => game.receiveVoiceFrame(buf);
// Per-tick game state arrives as binary (decoded in the socket) — see snapshotCodec.
socket.onSnapshot = (snap) => game.applySnapshot(snap);
let selfId: string | null = null;
let inMatch = false;
// Reconnect to a live match after a drop: the server hands us a per-match token
// (match_token) and pauses the duel when we drop; we retry rejoining until the
// grace window passes.
let matchToken: string | null = null;
let reconnecting = false;
let reconnectDeadline = 0;
let oppGraceTimer: ReturnType<typeof setInterval> | null = null;
let wallet: Address | null = null;
let wagerTerms: WagerTerms | null = null;
let createPublic = true;
let createWagered = false;
let createTokenKind: "usdc" | "eth" = "usdc"; // USDC is the simplest default
let createTier = 100; // USD cents — $1 default
let lobby: LobbyState | null = null;
let myChallenge: { code: string; key: string; role: "host" | "guest" } | null = null;
let qrCodeDrawnFor: string | null = null;
let rematchPending = false;
let lastMatchWagered = false;
// minimized = you stepped out of your DeathMatch to Home but it's still live.
// The Home card stands in for the lobby; you re-enter via OPEN / START.
let minimized = false;

// ── identity: callsign + challenge keys persist on this device ────────────
const NAME_KEY = "shotante.name";
const KEYS_KEY = "shotante.keys"; // { [code]: { key, role } }
const ACTIVE_KEY = "shotante.active"; // last challenge we host/joined

nameInput.value = localStorage.getItem(NAME_KEY) ?? "";
nameInput.addEventListener("input", () => localStorage.setItem(NAME_KEY, nameInput.value.trim()));

type StoredKey = { key: string; role: "host" | "guest" };
function allKeys(): Record<string, StoredKey> {
  try {
    return JSON.parse(localStorage.getItem(KEYS_KEY) ?? "{}");
  } catch {
    return {};
  }
}
function keyFor(code: string): StoredKey | null {
  return allKeys()[code] ?? null;
}
function storeKey(code: string, key: string, role: "host" | "guest"): void {
  const keys = allKeys();
  keys[code] = { key, role };
  localStorage.setItem(KEYS_KEY, JSON.stringify(keys));
  localStorage.setItem(ACTIVE_KEY, code);
}
function dropKey(code: string): void {
  const keys = allKeys();
  delete keys[code];
  localStorage.setItem(KEYS_KEY, JSON.stringify(keys));
  if (localStorage.getItem(ACTIVE_KEY) === code) localStorage.removeItem(ACTIVE_KEY);
}

function playerName(): string {
  return nameInput.value.trim() || "anon";
}

// Invite links: shotante.com/?join=CODE
const pendingJoin = new URLSearchParams(location.search).get("join")?.toUpperCase() ?? null;

function shareUrl(code: string): string {
  return SHARE_BASE
    ? `${SHARE_BASE.replace(/\/$/, "")}/c/${code}`
    : `${location.origin}${location.pathname}?join=${code}`;
}

// ── formatting helpers ─────────────────────────────────────────────────────
function fmtAmount(token: string | null, wei: string | null): string {
  if (wei === null) return "";
  const eth = token === null || token === NATIVE;
  const n = Number(wei) / (eth ? 1e18 : 1e6);
  return `${n.toLocaleString("en-US", { maximumFractionDigits: 6 })} ${eth ? "ETH" : "USDC"}`;
}

/** What the winner actually receives: the pot minus the protocol fee. */
function fmtNetPot(token: string | null, potWei: string | null, feeBps: number | null): string {
  if (potWei === null) return "";
  const net = (BigInt(potWei) * BigInt(10_000 - (feeBps ?? 0))) / 10_000n;
  return fmtAmount(token, net.toString());
}

/** Fixed-stake display in USD (the on-chain wei isn't known until match start,
 *  especially for ETH), e.g. "$1 USDC" / "$5 ETH". */
function fmtStake(usdCents: number | null, kind: "usdc" | "eth" | null): string {
  if (usdCents == null) return "";
  return `${fmtUsdCents(usdCents)} ${kind === "eth" ? "ETH" : "USDC"}`;
}
/** Approx USD the winner takes: 2× the ante minus the protocol fee. */
function fmtNetPotUsd(usdCents: number | null, feeBps: number | null): string {
  if (usdCents == null) return "";
  return fmtUsdCents(Math.round((usdCents * 2 * (10_000 - (feeBps ?? 0))) / 10_000));
}

function fmtCountdown(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (d > 0) return `${d}d ${h}h ${m}m`;
  if (h > 0) return `${h}h ${String(m).padStart(2, "0")}m ${String(sec).padStart(2, "0")}s`;
  return `${m}m ${String(sec).padStart(2, "0")}s`;
}

// ── panels ─────────────────────────────────────────────────────────────────
const PANELS = ["create-panel", "lobby-panel", "browse-panel"] as const;
type Panel = (typeof PANELS)[number] | null;

function showSubpanel(id: Panel): void {
  const wasBrowse = !$("browse-panel").classList.contains("hidden");
  for (const p of PANELS) $(p).classList.toggle("hidden", p !== id);
  $("btn-more").textContent = id === "create-panel" ? "MORE OPTIONS ▴" : "MORE OPTIONS ▾";
  if (wasBrowse && id !== "browse-panel" && socket.connected) socket.send({ type: "unsub_board" });
  refreshHomeUi();
}

function showMenu(message?: string): void {
  inMatch = false;
  reconnecting = false;
  matchToken = null;
  stopOpponentGrace();
  lobby = null;
  minimized = false;
  game.stop();
  game.startAttract(); // the live duel backdrop resumes with the menu
  socket.close();
  claimBtn.classList.add("hidden");
  rematchBtn.classList.add("hidden");
  showSubpanel(null);
  menu.classList.remove("hidden");
  if (message) status.textContent = message;
}

// ── home funnel: one DeathMatch at a time ─────────────────────────────────
// When a DeathMatch is in flight, Home hides the create/quick actions and
// shows the active-match card instead — so returning Home is never confusing.
function refreshHomeUi(): void {
  const hasMatch = !!myChallenge;
  const onHome = PANELS.every((p) => $(p).classList.contains("hidden"));
  $("btn-challenge").classList.toggle("hidden", hasMatch);
  $("btn-more").classList.toggle("hidden", hasMatch);
  $("home-actions").classList.toggle("hidden", hasMatch);
  $("hint-tap").classList.toggle("hidden", hasMatch);
  $("btn-wager").classList.toggle("hidden", hasMatch || !wallet);
  const showCard = hasMatch && onHome;
  $("active-match-card").classList.toggle("hidden", !showCard);
  if (showCard) renderActiveCard();
}

function renderActiveCard(): void {
  if (!myChallenge) return;
  const hasGuest = !!lobby?.guest;
  const wagered = !!lobby?.wagered;
  const ante = wagered && lobby ? ` · ⚔ ${fmtStake(lobby.usdCents, lobby.wagerTokenKind)} ante` : "";
  const primary = $("am-primary") as HTMLButtonElement;
  if (!lobby) {
    $("am-sub").textContent = "tap OPEN to return to your DeathMatch";
    primary.textContent = "OPEN";
  } else if (hasGuest) {
    $("am-sub").textContent = `⚔ opponent ready${ante}`;
    primary.textContent = "START";
  } else {
    $("am-sub").textContent =
      (myChallenge.role === "host"
        ? "waiting for your opponent to open the link"
        : "waiting for the host to start") + ante;
    primary.textContent = "OPEN";
  }
  const showLink = myChallenge.role === "host" && !hasGuest;
  $("am-link-row").classList.toggle("hidden", !showLink);
  ($("am-link") as HTMLInputElement).value = shareUrl(myChallenge.code);
  updateActiveCountdown();
}

function updateActiveCountdown(): void {
  const el = $("am-countdown");
  if (!myChallenge || !lobby || $("active-match-card").classList.contains("hidden")) {
    el.textContent = "";
    return;
  }
  const left = lobby.expiresAt - Date.now();
  el.textContent = left > 0 ? `expires in ${fmtCountdown(left)}` : "expired";
}

// Connection dropped. In a live match with a rejoin token, the duel is PAUSED
// server-side and we try to re-enter; otherwise it's a clean forfeit/return.
function handleSocketClose(): void {
  if (reconnecting) return; // the reconnect loop owns its own connects
  if (inMatch && matchToken) {
    void beginReconnect();
  } else if (inMatch) {
    showMenu("connection lost — match forfeited");
  } else if (myChallenge) {
    showMenu("connection lost — reopen your invite link to rejoin the lobby");
  }
}

const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

/** Retry reconnecting + rejoining the paused match until match_resumed clears
 *  `reconnecting`, or the grace window elapses (then forfeit to the menu). */
async function beginReconnect(): Promise<void> {
  if (reconnecting) return;
  reconnecting = true;
  reconnectDeadline = Date.now() + RECONNECT_GRACE_MS;
  game.showStatusBanner("RECONNECTING…");
  while (reconnecting && Date.now() < reconnectDeadline) {
    try {
      if (!socket.connected) await socket.connect(handleServerMessage, handleSocketClose);
      if (matchToken) socket.send({ type: "rejoin_match", token: matchToken });
    } catch {
      /* server unreachable — keep trying until the deadline */
    }
    await sleep(1200);
  }
  if (reconnecting) {
    reconnecting = false;
    game.showStatusBanner(null);
    showMenu("match forfeited — couldn't reconnect in time");
  }
}

// Stayer's view while the opponent is reconnecting: a ticking countdown banner.
function startOpponentGrace(graceMs: number): void {
  stopOpponentGrace();
  const until = Date.now() + graceMs;
  const tick = (): void => {
    const left = Math.ceil((until - Date.now()) / 1000);
    game.showStatusBanner(
      left > 0
        ? `OPPONENT DROPPED<br><span style="font-size:55%">reconnecting… ${left}s</span>`
        : "OPPONENT DROPPED",
    );
    if (left <= 0) stopOpponentGrace();
  };
  tick();
  oppGraceTimer = setInterval(tick, 1000);
}
function stopOpponentGrace(): void {
  if (oppGraceTimer) {
    clearInterval(oppGraceTimer);
    oppGraceTimer = null;
  }
}

async function ensureSocket(): Promise<boolean> {
  if (socket.connected) return true;
  try {
    await socket.connect(handleServerMessage, handleSocketClose);
    return true;
  } catch {
    status.textContent = "game server offline — try PRACTICE, or start the server";
    return false;
  }
}

// ── basic actions ─────────────────────────────────────────────────────────
$("btn-practice").addEventListener("click", () => {
  menu.classList.add("hidden");
  game.startPractice();
  if (!TOUCH) canvas.requestPointerLock?.();
});

$("btn-duel").addEventListener("click", async () => {
  if (!(await ensureSocket())) return;
  socket.send({ type: "queue", name: playerName() });
});

$("btn-wager").addEventListener("click", async () => {
  if (!wallet || !(await ensureSocket())) return;
  socket.send({ type: "queue", name: playerName(), wagerAddress: wallet, wagerTier: createTier, wagerToken: createTokenKind });
});

// Shared by both connect paths (Base Account + injected extension wallets).
function onWalletConnected(addr: string, label: string): void {
  wallet = addr as Address;
  $("btn-wallet").textContent = `WALLET ${shortAddress(addr)}`;
  ($("opt-wagered") as HTMLButtonElement).disabled = false;
  $("wallet-pick").classList.add("hidden");
  refreshHomeUi();
  status.textContent = `${label} connected — wagered games unlocked`;
}

$("btn-wallet").addEventListener("click", async () => {
  const addr = await connectWallet();
  if (addr) onWalletConnected(addr, "Base Account");
  else $("btn-wallet").textContent = "SIGN-IN CANCELLED — RETRY";
});

// Connect an injected extension wallet (MetaMask, Rabby, …) via EIP-6963.
$("btn-wallet-ext").addEventListener("click", async () => {
  status.textContent = "looking for browser wallets…";
  const wallets = await listInjectedWallets();
  const pick = $("wallet-pick");
  pick.replaceChildren();

  if (wallets.length === 0) {
    pick.classList.add("hidden");
    status.textContent = "no browser wallet found — install MetaMask or Rabby, then retry";
    return;
  }

  const connect = async (w: InjectedWallet) => {
    status.textContent = `connecting ${w.name}…`;
    const addr = await connectInjected(w);
    if (addr) onWalletConnected(addr, w.name);
    else status.textContent = `${w.name} connection cancelled — retry`;
  };

  if (wallets.length === 1) {
    await connect(wallets[0]);
    return;
  }

  // Multiple wallets installed — let the player choose which one.
  for (const w of wallets) {
    const b = document.createElement("button");
    b.className = "btn ghost";
    b.textContent = w.name.toUpperCase();
    b.addEventListener("click", () => connect(w));
    pick.appendChild(b);
  }
  pick.classList.remove("hidden");
  status.textContent = "pick a wallet to connect";
});

// ── one-tap challenge: tap → private lobby + link, nothing to configure ──
let autoCopyLink = false;

$("btn-challenge").addEventListener("click", async () => {
  if (!(await ensureSocket())) return;
  autoCopyLink = true;
  socket.send({ type: "create_challenge", name: playerName(), isPublic: false });
  status.textContent = "setting up your DeathMatch…";
});

// ── advanced create (public / wagered) behind MORE OPTIONS ────────────────
$("btn-more").addEventListener("click", () => {
  const open = $("create-panel").classList.contains("hidden");
  showSubpanel(open ? "create-panel" : null);
});

function setToggle(onId: string, offId: string, wager = false): void {
  $(onId).classList.add("on");
  $(offId).classList.remove("on");
  if (wager) $(onId).classList.add("wager-on");
}
$("opt-public").addEventListener("click", () => { createPublic = true; setToggle("opt-public", "opt-private"); });
$("opt-private").addEventListener("click", () => { createPublic = false; setToggle("opt-private", "opt-public"); });
$("opt-free").addEventListener("click", () => { createWagered = false; setToggle("opt-free", "opt-wagered"); refreshWagerOpts(); });
$("opt-wagered").addEventListener("click", () => { createWagered = true; setToggle("opt-wagered", "opt-free", true); refreshWagerOpts(); });

// fixed-stake selection: token (USDC / Base ETH) + USD tier
function fmtUsdCents(c: number): string {
  return c % 100 === 0 ? `$${c / 100}` : `$${(c / 100).toFixed(2)}`;
}
function refreshWagerOpts(): void {
  $("wager-opts").classList.toggle("hidden", !createWagered);
  const tok = createTokenKind === "usdc" ? "USDC" : "Base ETH";
  $("wager-hint").textContent = `${fmtUsdCents(createTier)} in ${tok} each · winner takes the pot`;
}
$("tok-usdc").addEventListener("click", () => { createTokenKind = "usdc"; setToggle("tok-usdc", "tok-eth"); refreshWagerOpts(); });
$("tok-eth").addEventListener("click", () => { createTokenKind = "eth"; setToggle("tok-eth", "tok-usdc"); refreshWagerOpts(); });
for (const btn of document.querySelectorAll<HTMLButtonElement>("#tier-row .tier")) {
  btn.addEventListener("click", () => {
    createTier = Number(btn.dataset.cents);
    for (const b of document.querySelectorAll("#tier-row .tier")) b.classList.toggle("on", b === btn);
    refreshWagerOpts();
  });
}

$("btn-create-go").addEventListener("click", async () => {
  if (createWagered && !wallet) {
    status.textContent = "connect a wallet to host a wagered DeathMatch";
    return;
  }
  if (!(await ensureSocket())) return;
  socket.send({
    type: "create_challenge",
    name: playerName(),
    isPublic: createPublic,
    ...(createWagered && wallet ? { wagerAddress: wallet, wagerTier: createTier, wagerToken: createTokenKind } : {}),
  });
});

// ── lobby (shared waiting room) ───────────────────────────────────────────
// ← BACK: minimize, keep the DeathMatch alive. Host stays attached but not
// ready (so a friend opening the link prompts instead of hard-starting); guest
// detaches but keeps its slot. Either way it lands on the Home card.
function backToHome(): void {
  if (!myChallenge) return;
  minimized = true;
  if (socket.connected) {
    if (myChallenge.role === "host") socket.send({ type: "lobby_ready", ready: false });
    else socket.send({ type: "leave_lobby" });
  }
  showSubpanel(null);
  status.textContent = "your DeathMatch is live on your home screen — send the link or step back in";
}
$("btn-back-lobby").addEventListener("click", backToHome);

// CANCEL: destructive. Kills the DeathMatch (host) or gives up the slot (guest).
function cancelMatch(): void {
  if (!myChallenge) return;
  if (lobby?.wagered && !confirm("Cancel this wagered DeathMatch? Your ante is refunded to your claimable balance.")) return;
  if (myChallenge.role === "host") {
    socket.send({ type: "cancel_challenge", code: myChallenge.code, hostKey: myChallenge.key });
  } else if (socket.connected) {
    socket.send({ type: "leave_lobby" });
  }
  dropKey(myChallenge.code);
  myChallenge = null;
  showMenu("DeathMatch cancelled");
}
$("btn-cancel-room").addEventListener("click", cancelMatch);
$("am-cancel").addEventListener("click", cancelMatch);

// Home card primary: OPEN (re-enter the lobby) or START (begin the duel).
$("am-primary").addEventListener("click", async () => {
  if (!myChallenge) return;
  const stillAttached = socket.connected && minimized && myChallenge.role === "host";
  minimized = false;
  if (!(await ensureSocket())) return;
  if (stillAttached) {
    // host stepped away but the socket stayed open — just re-ready. If the
    // opponent is already here this starts the duel; otherwise reopens the lobby.
    socket.send({ type: "lobby_ready", ready: true });
    if (lobby) renderLobby(lobby);
  } else {
    // guest, or a host whose socket dropped: re-attach via the stored key
    // (play-now auto-readies on attach, so this also makes the host ready).
    socket.send({
      type: "reclaim_challenge",
      code: myChallenge.code,
      key: myChallenge.key,
      name: playerName(),
      ...(lobby?.wagered && wallet ? { wagerAddress: wallet } : {}),
    });
    status.textContent = "stepping back into your DeathMatch…";
  }
});

$("am-copy").addEventListener("click", async () => {
  if (!myChallenge) return;
  const url = shareUrl(myChallenge.code);
  try {
    await navigator.clipboard.writeText(url);
    $("am-copy").textContent = "COPIED ✓";
    setTimeout(() => ($("am-copy").textContent = "COPY"), 2000);
  } catch {
    const el = $("am-link") as HTMLInputElement;
    el.focus();
    el.select();
  }
});

$("btn-share").addEventListener("click", async () => {
  if (!myChallenge) return;
  const url = shareUrl(myChallenge.code);
  const text = lobby?.wagered
    ? `I'm calling you out — ${fmtStake(lobby.usdCents, lobby.wagerTokenKind)} ante, winner takes the pot.`
    : "I'm calling you out. 1v1 me on ShotAnte.";
  try {
    if (navigator.share) await navigator.share({ title: "ShotAnte — you've been challenged", text, url });
    else {
      await navigator.clipboard.writeText(url);
      ($("btn-share") as HTMLButtonElement).textContent = "LINK COPIED ✓";
    }
  } catch {
    /* user dismissed the sheet */
  }
});

$("btn-copy-invite").addEventListener("click", async () => {
  if (!myChallenge) return;
  const url = shareUrl(myChallenge.code);
  const linkInput = $("invite-link") as HTMLInputElement;
  try {
    await navigator.clipboard.writeText(url);
    $("btn-copy-invite").textContent = "COPIED ✓";
    setTimeout(() => ($("btn-copy-invite").textContent = "COPY"), 2000);
  } catch {
    linkInput.focus();
    linkInput.select(); // clipboard blocked: pre-select so cmd+C works
  }
});

function renderSlot(elId: string, slot: { name: string; present: boolean; ready: boolean } | null): void {
  const el = $(elId);
  const nameEl = el.querySelector(".sl-name")!;
  const stateEl = el.querySelector(".sl-state")!;
  el.classList.remove("ready", "absent");
  if (!slot) {
    nameEl.textContent = "waiting…";
    stateEl.textContent = "";
    el.classList.add("absent");
    return;
  }
  nameEl.textContent = slot.name;
  if (!slot.present) {
    stateEl.textContent = "OFFLINE";
    el.classList.add("absent");
  } else {
    stateEl.textContent = slot.ready ? "READY" : "HERE";
    el.classList.add("ready");
  }
}

function renderLobby(lb: LobbyState): void {
  const isNewLobby = lobby?.code !== lb.code;
  const hadGuest = !!lobby?.guest;
  lobby = lb;
  if (isNewLobby) {
    $("chat-msgs").innerHTML = "";
    $("btn-copy-invite").textContent = "COPY";
    ($("btn-share") as HTMLButtonElement).textContent = "SHARE LINK";
  }
  renderSlot("slot-host", lb.host);
  renderSlot("slot-guest", lb.guest);

  // The link IS the lobby: hosts see it big until an opponent shows up,
  // guests never need it.
  const amHost = myChallenge?.role !== "guest";
  const hasGuest = lb.guest !== null;
  const hostReady = lb.host.present && lb.host.ready;
  ($("invite-link") as HTMLInputElement).value = shareUrl(lb.code);
  $("share-block").classList.toggle("hidden", !amHost || hasGuest);
  $("lobby-title").textContent = !hasGuest
    ? amHost
      ? "SEND THIS LINK TO YOUR FRIEND"
      : "WAITING FOR THE HOST…"
    : amHost || hostReady
      ? "OPPONENT FOUND — STARTING…"
      : "HOST STEPPED AWAY";

  const wager = lb.wagered
    ? ` · ⚔️ ${fmtStake(lb.usdCents, lb.wagerTokenKind)} ante — winner takes ~${fmtNetPotUsd(lb.usdCents, lb.feeBps)}`
    : "";
  $("room-hint").textContent = !hasGuest
    ? `the duel starts the moment they open it${lb.isPublic ? " · also listed on the open board" : ""}${wager}`
    : amHost || hostReady
      ? `opponent's here — starting…${wager}`
      : `the host stepped away — they'll start the duel${wager}`;

  if (myChallenge && qrCodeDrawnFor !== lb.code) {
    qrCodeDrawnFor = lb.code;
    void QRCode.toCanvas($("qr") as HTMLCanvasElement, shareUrl(lb.code), {
      width: 116,
      margin: 1,
      color: { dark: "#0a0a12", light: "#ffffff" },
    });
  }
  ($("btn-cancel-room") as HTMLButtonElement).classList.toggle("hidden", !amHost);
  ($("btn-back-lobby") as HTMLButtonElement).textContent = amHost ? "← BACK" : "← LEAVE (SLOT SAVED)";

  if (minimized) {
    // stepped away: keep the lobby DOM fresh but stay on the Home card, and
    // nudge when an opponent arrives so the host knows they can START.
    refreshHomeUi();
    if (hasGuest && !hadGuest && amHost) {
      status.textContent = "⚔ your opponent is here — tap START";
      game.uiClick();
    }
  } else {
    showSubpanel("lobby-panel");
  }
}

// live countdown for the Home DeathMatch card — ticks locally, no server traffic
setInterval(updateActiveCountdown, 1000);

// ── lobby chat (ephemeral) ────────────────────────────────────────────────
let lastChatSent = 0;

function appendLobbyChat(from: string, text: string, self: boolean): void {
  const box = $("chat-msgs");
  const line = document.createElement("div");
  if (self) line.className = "cm-self";
  const f = document.createElement("span");
  f.className = "cm-from";
  f.textContent = `${from}: `;
  line.appendChild(f);
  line.appendChild(document.createTextNode(text));
  box.appendChild(line);
  while (box.children.length > 30) box.firstChild?.remove();
  box.scrollTop = box.scrollHeight;
}

$("lobby-chat-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const input = $("lobby-chat-input") as HTMLInputElement;
  const text = input.value.trim().slice(0, 120);
  if (!text || Date.now() - lastChatSent < 1000 || !socket.connected) return;
  lastChatSent = Date.now();
  socket.send({ type: "chat", text });
  appendLobbyChat(playerName(), text, true);
  input.value = "";
});

// ── upcoming duels board ──────────────────────────────────────────────────
$("btn-browse").addEventListener("click", async () => {
  const open = $("browse-panel").classList.contains("hidden");
  showSubpanel(open ? "browse-panel" : null);
  if (open && (await ensureSocket())) socket.send({ type: "sub_board" });
});

function joinFromBoard(c: ChallengeInfo): void {
  if (c.wagered && !wallet) {
    status.textContent = "connect a wallet to join wagered DeathMatches";
    return;
  }
  socket.send({
    type: "join_challenge",
    code: c.code,
    name: playerName(),
    ...(c.wagered && wallet ? { wagerAddress: wallet } : {}),
  });
  status.textContent = `joining ${c.hostName}'s DeathMatch…`;
}

function boardRow(c: ChallengeInfo): HTMLElement {
  const row = document.createElement("div");
  row.className = "room-row";
  const name = document.createElement("span");
  name.className = "rr-name";
  name.textContent = c.hostName;
  const sub = document.createElement("span");
  sub.className = "rr-sub";
  sub.textContent = `open ${Math.max(1, Math.round(c.ageSeconds / 60))}m`;
  name.appendChild(sub);
  row.appendChild(name);
  if (c.wagered) {
    const tag = document.createElement("span");
    tag.className = "rr-tag";
    tag.textContent = `⚔️ ${fmtStake(c.usdCents, c.wagerTokenKind)}`;
    row.appendChild(tag);
  }
  const btn = document.createElement("button");
  btn.className = "btn";
  if (c.status === "matched") {
    btn.textContent = "FULL";
    btn.disabled = true;
  } else {
    btn.textContent = "JOIN";
    btn.onclick = () => joinFromBoard(c);
  }
  row.appendChild(btn);
  return row;
}

function renderBoard(upcoming: ChallengeInfo[]): void {
  const open = $("board-open");
  const now = upcoming.filter((c) => c.scheduledAt === null);
  open.innerHTML = "";
  if (!now.length) open.innerHTML = `<p class="hint">no open deathmatches — start one and share the link</p>`;
  else for (const c of now) open.appendChild(boardRow(c));
}

// ── invite landing card (?join=CODE) ──────────────────────────────────────
async function fetchChallenge(code: string): Promise<ChallengeInfo | null> {
  try {
    const res = await fetch(`${API_URL}/api/challenge/${code}`);
    return res.ok ? ((await res.json()) as ChallengeInfo) : null;
  } catch {
    return null;
  }
}

function sendJoin(code: string, info: ChallengeInfo | null): void {
  const stored = keyFor(code);
  if (stored) {
    socket.send({
      type: "reclaim_challenge",
      code,
      key: stored.key,
      name: playerName(),
      ...(info?.wagered && wallet ? { wagerAddress: wallet } : {}),
    });
    status.textContent = `rejoining ${code}…`;
    return;
  }
  socket.send({
    type: "join_challenge",
    code,
    name: playerName(),
    ...(info?.wagered && wallet ? { wagerAddress: wallet } : {}),
  });
  status.textContent = `joining DeathMatch ${code}…`;
}

// Wallet-based reclaim (wagered): prove control of the slot's wallet from a new
// device with no stored key. Ask the server for a nonce, sign it, send it back.
async function requestWalletReclaim(code: string): Promise<void> {
  if (!wallet || !(await ensureSocket())) return;
  socket.send({ type: "reclaim_nonce_request", code, wagerAddress: wallet });
  status.textContent = "requesting a sign-in challenge for your wallet…";
}

async function signAndReclaim(code: string, role: "host" | "guest", message: string): Promise<void> {
  if (!wallet) return;
  try {
    const { getProvider } = await import("./wallet/provider.ts");
    const provider = await getProvider();
    const signature = (await provider.request({
      method: "personal_sign",
      params: [message, wallet],
    })) as string;
    if (!(await ensureSocket())) return;
    socket.send({ type: "reclaim_challenge", code, key: "", name: playerName(), wagerAddress: wallet, signature });
    status.textContent = `verifying your wallet to rejoin as ${role}…`;
  } catch {
    status.textContent = "wallet signature cancelled — you can try again";
  }
}

async function showInviteCard(code: string): Promise<void> {
  const card = $("invite-card");
  const title = $("ic-title");
  const sub = $("ic-sub");
  const joinBtn = $("ic-join") as HTMLButtonElement;
  card.classList.remove("hidden");

  const info = await fetchChallenge(code);
  const stored = keyFor(code);

  if (!info) {
    title.textContent = "DEATHMATCH NOT FOUND";
    sub.textContent = `code ${code} — it may have finished or expired. You can still try joining.`;
    joinBtn.textContent = `TRY JOIN ${code}`;
  } else {
    title.textContent = `${info.hostName.toUpperCase()} CHALLENGES YOU`;
    sub.textContent = info.wagered
      ? `⚔️ ${fmtStake(info.usdCents, info.wagerTokenKind)} ante — winner takes ~${fmtNetPotUsd(info.usdCents, info.feeBps)}${info.feeBps ? ` (after ${info.feeBps / 100}% fee)` : ""}`
      : "free duel — no wallet needed · ready when you are";
    if (stored) joinBtn.textContent = "REJOIN THE DEATHMATCH";
    else if (info.status === "matched" || info.status === "live") {
      joinBtn.textContent = info.status === "live" ? "DUEL IN PROGRESS" : "DEATHMATCH FULL";
      joinBtn.disabled = true;
      sub.textContent += " · this one already has an opponent";
    } else joinBtn.textContent = "JOIN THE DUEL";
  }

  joinBtn.onclick = async () => {
    if (info?.wagered && !wallet) {
      status.textContent = "this is a wagered duel — connect your wallet first";
      return;
    }
    if (!(await ensureSocket())) return;
    sendJoin(code, info);
  };

  // Returning wagered player on a new device (no stored key): rejoin by proving
  // wallet control instead of the device key.
  const walletRejoinBtn = $("ic-wallet-rejoin") as HTMLButtonElement;
  const canWalletRejoin = !!info?.wagered && !!wallet && !stored;
  walletRejoinBtn.classList.toggle("hidden", !canWalletRejoin);
  walletRejoinBtn.onclick = () => void requestWalletReclaim(code);

  // One-tap becomes zero-tap: free challenge + a saved callsign and no wager
  // confirmation needed -> walk straight into the lobby.
  if (
    info &&
    !stored &&
    !info.wagered &&
    info.status === "open" &&
    localStorage.getItem(NAME_KEY)
  ) {
    status.textContent = `auto-joining ${info.hostName}'s duel as ${playerName()}…`;
    if (await ensureSocket()) sendJoin(code, info);
  }
}

// ── rematch ───────────────────────────────────────────────────────────────
rematchBtn.addEventListener("click", async () => {
  rematchBtn.disabled = true;
  game.stop();
  menu.classList.remove("hidden");
  claimBtn.classList.add("hidden");
  rematchBtn.classList.add("hidden");
  rematchBtn.disabled = false;
  if (!(await ensureSocket())) return;
  rematchPending = true;
  inMatch = false;
  socket.send({
    type: "create_challenge",
    name: playerName(),
    isPublic: false,
    ...(lastMatchWagered && wallet ? { wagerAddress: wallet } : {}),
  });
  status.textContent = "opening a rematch challenge…";
});

// ── server messages ───────────────────────────────────────────────────────
function handleServerMessage(msg: ServerMessage): void {
  switch (msg.type) {
    case "welcome":
      // on a reconnect the new connection gets a fresh id, but we keep our match
      // identity (the server rebinds us by token), so don't overwrite selfId
      if (!reconnecting) selfId = msg.playerId;
      break;
    case "ping":
      // echo straight back so the server can measure our RTT (lag compensation)
      socket.send({ type: "pong", t: msg.t });
      break;
    case "match_token":
      matchToken = msg.token; // our key to re-enter this match after a drop
      break;
    case "match_resumed":
      // we're back in the paused duel — stop reconnecting, clear the overlay
      reconnecting = false;
      game.showStatusBanner(null);
      break;
    case "opponent_dropped":
      // the OTHER player dropped — show a reconnect countdown until they return
      startOpponentGrace(msg.graceMs);
      break;
    case "opponent_reconnected":
      stopOpponentGrace();
      game.showStatusBanner(null);
      break;
    case "queued":
      status.textContent = msg.wagered
        ? "in wagered queue — waiting for an opponent…"
        : "in queue — waiting for an opponent…";
      break;

    case "challenge_created": {
      myChallenge = { code: msg.code, key: msg.hostKey, role: "host" };
      storeKey(msg.code, msg.hostKey, "host");
      $("invite-card").classList.add("hidden");
      status.textContent = "DeathMatch open — waiting for an opponent…";
      refreshHomeUi();
      if (autoCopyLink) {
        autoCopyLink = false;
        void navigator.clipboard
          .writeText(shareUrl(msg.code))
          .then(() => (status.textContent = "link copied ✓ — paste it to your friend, the duel starts when they open it"))
          .catch(() => {}); // link is shown in the lobby anyway
      }
      if (rematchPending) {
        rematchPending = false;
        void navigator.clipboard
          .writeText(shareUrl(msg.code))
          .then(() => (status.textContent = "rematch link copied — send it to your opponent"))
          .catch(() => {});
      }
      break; // lobby_state arrives right behind and renders the panel
    }
    case "challenge_reserved": {
      myChallenge = { code: msg.code, key: msg.guestKey, role: "guest" };
      storeKey(msg.code, msg.guestKey, "guest");
      $("invite-card").classList.add("hidden");
      status.textContent = "joined — waiting for the duel to start…";
      refreshHomeUi();
      break;
    }
    case "lobby_state":
      // we may be the host reclaiming: adopt role from stored keys
      if (!myChallenge) {
        const stored = keyFor(msg.lobby.code);
        if (stored) myChallenge = { code: msg.lobby.code, key: stored.key, role: stored.role };
      }
      renderLobby(msg.lobby);
      break;
    case "challenge_cancelled": {
      dropKey(msg.code);
      if (myChallenge?.code === msg.code) {
        myChallenge = null;
        lobby = null;
        minimized = false;
        showSubpanel(null);
        status.textContent =
          msg.reason === "expired"
            ? "DeathMatch expired — start a new one"
            : "DeathMatch cancelled";
      }
      break;
    }
    case "reclaim_nonce":
      void signAndReclaim(msg.code, msg.role, msg.message);
      break;
    case "board_update":
      renderBoard(msg.upcoming);
      break;

    case "chat":
      // In-match proximity chat is voice now; only lobby text chat remains.
      if (msg.scope === "lobby") appendLobbyChat(msg.from, msg.text, false);
      break;

    case "escrow_action": {
      wagerTerms = msg.terms;
      void runEscrowAction(msg.action, msg.terms);
      break;
    }
    case "escrow_status": {
      if (msg.status === "locked") status.textContent = "escrow locked ✓ — duel starting…";
      else if (msg.status === "failed") showMenu(`wager failed: ${msg.detail ?? "unknown"}`);
      else if (msg.detail) status.textContent = msg.detail;
      break;
    }

    case "match_start": {
      inMatch = true;
      reconnecting = false;
      matchToken = null; // a fresh match_token arrives right after this
      stopOpponentGrace();
      game.showStatusBanner(null);
      lastMatchWagered = msg.wagered;
      if (myChallenge) {
        dropKey(myChallenge.code); // the challenge is consumed — no reclaim
        myChallenge = null;
      }
      lobby = null;
      showSubpanel(null);
      $("invite-card").classList.add("hidden");
      menu.classList.add("hidden");
      game.onInput = (input: PlayerInput) => socket.send({ type: "input", input });
      game.startDuel(selfId ?? "", msg.players, msg.countdownMs, msg.mapSeed);
      if (!TOUCH) canvas.requestPointerLock?.();
      break;
    }
    case "snapshot":
      game.applySnapshot(msg.snapshot);
      break;

    case "match_end": {
      inMatch = false;
      reconnecting = false;
      matchToken = null;
      stopOpponentGrace();
      game.showMatchEnd(msg.result, selfId);
      rematchBtn.classList.remove("hidden");
      if (
        msg.settlement &&
        wagerTerms &&
        wallet &&
        msg.settlement.winnerAddress.toLowerCase() === wallet.toLowerCase()
      ) {
        const settlement = msg.settlement;
        const terms = wagerTerms;
        if (settlement.payout === "sent") {
          // Server relayed settle() — the pot was pushed straight to the
          // winner's wallet. Nothing for them to sign or pay.
          document.exitPointerLock?.();
          setTimeout(() => showMenu("you won — pot sent to your wallet ✓ · rematch?"), 1500);
        } else {
          // Fallback (no relayer / relay failed): the winner submits the signed
          // result and pulls the pot themselves.
          claimBtn.classList.remove("hidden");
          claimBtn.onclick = async () => {
            claimBtn.disabled = true;
            try {
              await (await escrow()).submitAndClaim(
                terms,
                settlement.winnerAddress as Address,
                settlement.signature as Hex,
                (s) => (claimBtn.textContent = s.toUpperCase()),
              );
              setTimeout(() => showMenu("pot claimed ✓ — rematch?"), 1500);
            } catch {
              claimBtn.textContent = "CLAIM FAILED — RETRY";
              claimBtn.disabled = false;
            }
          };
          document.exitPointerLock?.();
        }
      } else {
        setTimeout(() => {
          // stay on the end screen if the player is deciding on a rematch
          if (!inMatch && menu.classList.contains("hidden") && rematchPending === false) {
            showMenu("rematch? find another duel");
            rematchBtn.classList.remove("hidden");
          }
        }, 6000);
        document.exitPointerLock?.();
      }
      break;
    }
    case "error":
      status.textContent = msg.message;
      break;
  }
}

async function runEscrowAction(action: "create" | "join", terms: WagerTerms): Promise<void> {
  if (!wallet) return;
  try {
    status.textContent =
      action === "create"
        ? "anteing in (create match) — confirm in your wallet…"
        : "opponent anted — matching their ante, confirm in your wallet…";
    const esc = await escrow();
    if (action === "create") await esc.sendCreate(terms, wallet);
    else await esc.sendJoin(terms, wallet);
    status.textContent = "ante locked in — waiting for the duel to start…";
  } catch {
    status.textContent = "ante transaction failed/rejected — leaving queue";
    socket.send({ type: "leave_queue" });
    if (action === "create" && wagerTerms) {
      try {
        await (await escrow()).cancelEscrow(wagerTerms, wallet);
        status.textContent = "ante cancelled and refunded to claimable balance";
      } catch {
        /* nothing was created — fine */
      }
    }
  }
}

// ── proximity voice: mic permission + self-mute ───────────────────────────
// Proximity VOICE is on in every match. We grant the mic ONCE from the homepage
// banner tap (a real user gesture — the reliable cross-platform path on iOS,
// Android and desktop) and hold the stream (track disabled) until a match
// starts, so there's no in-match re-prompt.
const micWarning = $("mic-warning") as HTMLButtonElement;

function renderMicWarning(): void {
  const perm = game.micPermission();
  if (perm === "granted") {
    micWarning.classList.add("hidden");
  } else if (perm === "denied") {
    micWarning.textContent = "🔇 Mic blocked — you can still hear, but not talk (tap to retry)";
    micWarning.classList.remove("hidden");
  } else {
    micWarning.textContent = "🎙️ Enable your mic for proximity voice chat";
    micWarning.classList.remove("hidden");
  }
}

micWarning.addEventListener("click", async () => {
  game.unlockAudio(); // also primes Web Audio output on the same gesture
  await game.enableMic(); // the #menu click handler already plays the UI blip
  renderMicWarning();
});

renderMicWarning();

// In-game mic pill: tap to self-mute / unmute for privacy.
$("mic").addEventListener("click", () => {
  if (game.micPermission() !== "granted") {
    void game.enableMic().then(renderMicWarning);
    return;
  }
  game.toggleSelfMute();
  game.uiClick();
});

// ── sound ───────────────────────────────────────────────────────────────────
// Sound is always on; there's no in-app mute (players use the phone's silent
// switch). The ONLY thing we must do is unlock the AudioContext, because mobile
// browsers — including older iOS Safari — start it suspended until a real user
// gesture. We resume + prime it on the first interaction anywhere, so the very
// first game sound (which fires outside a gesture) isn't silently dropped.
let audioUnlocked = false;
function unlockAudioOnce(): void {
  if (audioUnlocked) return;
  audioUnlocked = true;
  game.unlockAudio();
}
for (const evt of ["pointerdown", "touchend", "click", "keydown"] as const) {
  window.addEventListener(evt, unlockAudioOnce, { once: true });
}

// A blip on menu button presses (also a fine unlock gesture).
menu.addEventListener("click", (e) => {
  if ((e.target as HTMLElement).closest("button")) game.uiClick();
});

// ── upcoming-duels ticker (menu): live board data in one line ──────────────
const tickerEl = $("ticker");
async function refreshTicker(): Promise<void> {
  if (menu.classList.contains("hidden")) return;
  try {
    const res = await fetch(`${API_URL}/api/upcoming`);
    if (!res.ok) throw new Error();
    const list = (await res.json()) as ChallengeInfo[];
    const open = list.filter((c) => c.status === "open");
    if (open.length === 0) {
      tickerEl.classList.add("hidden");
      return;
    }
    const staked = open.filter((c) => c.usdCents != null).sort((a, b) => (b.usdCents ?? 0) - (a.usdCents ?? 0));
    const top = staked[0];
    tickerEl.textContent =
      `▸ ${open.length} OPEN DEATHMATCH${open.length === 1 ? "" : "ES"} ON THE BOARD` +
      (top ? ` · TOP STAKE ${fmtStake(top.usdCents, top.wagerTokenKind)}` : "") +
      " — CLICK TO BROWSE";
    tickerEl.classList.remove("hidden");
  } catch {
    tickerEl.classList.add("hidden"); // server offline — just hide the strip
  }
}
tickerEl.addEventListener("click", () => ($("btn-browse") as HTMLButtonElement).click());
setInterval(() => void refreshTicker(), 30_000);
void refreshTicker();

// ── boot: invite landing / lobby auto-reclaim ─────────────────────────────
game.startAttract(); // the menu opens onto a live duel, not a black void
// dev affordance: ?practice=1 jumps straight into a practice arena (also lets
// headless screenshot runs capture the in-game view without a click)
if (new URLSearchParams(location.search).has("practice")) {
  menu.classList.add("hidden");
  game.startPractice();
}
// ?replay=<matchId> — fetch a recorded match and play it back (spectator)
const replayId = new URLSearchParams(location.search).get("replay");
if (replayId) {
  menu.classList.add("hidden");
  void (async () => {
    try {
      const res = await fetch(`${API_URL}/api/replay/${encodeURIComponent(replayId)}`);
      if (!res.ok) throw new Error();
      const doc = await res.json();
      const frames = (doc.frames as { t: number; b: string }[]).map((f) => ({
        t: f.t,
        bytes: Uint8Array.from(atob(f.b), (c) => c.charCodeAt(0)),
      }));
      game.startReplay({ mapSeed: doc.mapSeed, players: doc.players, countdownMs: doc.countdownMs }, frames);
    } catch {
      showMenu("replay not found");
    }
  })();
}
if (pendingJoin) {
  void showInviteCard(pendingJoin);
} else {
  // Refreshed mid-lobby? Walk back in via the stored key.
  const active = localStorage.getItem(ACTIVE_KEY);
  const stored = active ? keyFor(active) : null;
  if (active && stored) {
    void (async () => {
      const info = await fetchChallenge(active);
      if (!info || (info.status !== "open" && info.status !== "matched")) {
        dropKey(active);
        return;
      }
      if (info.wagered && !wallet) {
        status.textContent = `your wagered DeathMatch ${active} is waiting — connect your wallet, then reopen your invite link`;
        return;
      }
      if (await ensureSocket()) {
        socket.send({ type: "reclaim_challenge", code: active, key: stored.key, name: playerName() });
        status.textContent = `rejoining your DeathMatch ${active}…`;
      }
    })();
  }
}

// ESC from pointer lock returns to menu when not in a live match
document.addEventListener("pointerlockchange", () => {
  if (!document.pointerLockElement && !inMatch && menu.classList.contains("hidden")) {
    showMenu();
  }
});
