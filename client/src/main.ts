// ZombieZap entry: menu → lobby → match, speaking zz-server's dialect.
// Deliberately small — the game lives in ZzGame.ts, the wire in shared/.

import "./style.css";
import { GameSocket } from "./network/socket.ts";
import { ZzGame } from "./game/ZzGame.ts";
import type { RosterPlayer, ServerMessage } from "@shotante/shared";

const $ = <T extends HTMLElement = HTMLElement>(id: string): T =>
  document.getElementById(id) as T;

const menu = $("menu");
const lobbyPanel = $("lobby");
const hud = $("hud");
const banner = $("zz-banner");
const overrun = $("overrun");
const canvas = $<HTMLCanvasElement>("game");

const nameInput = $<HTMLInputElement>("name");
const codeInput = $<HTMLInputElement>("code");
const menuError = $("menu-error");

nameInput.value = localStorage.getItem("zz-name") ?? "survivor";

const socket = new GameSocket();
let game: ZzGame | null = null;
let myId = "";
let hostId = "";
let inLobbyCode = "";
let bannerTimer: ReturnType<typeof setTimeout> | null = null;

function show(el: HTMLElement, on: boolean): void {
  el.classList.toggle("hidden", !on);
}

function setBanner(text: string, ms = 2500): void {
  banner.textContent = text;
  show(banner, true);
  if (bannerTimer) clearTimeout(bannerTimer);
  bannerTimer = setTimeout(() => show(banner, false), ms);
}

function fail(message: string): void {
  menuError.textContent = message;
  show(menuError, true);
}

async function ensureConnected(): Promise<boolean> {
  if (socket.connected) return true;
  try {
    await socket.connect(onMessage, () => {
      // dropped: back to the menu, keep it honest
      teardownMatch();
      show(menu, true);
      show(lobbyPanel, false);
      fail("connection lost");
    });
    socket.send({ type: "hello", name: nameInput.value.trim() || "survivor" });
    return true;
  } catch {
    fail("could not reach the game server");
    return false;
  }
}

function teardownMatch(): void {
  game?.dispose();
  game = null;
  show(hud, false);
  show(overrun, false);
  show(banner, false);
}

// ── menu actions ─────────────────────────────────────────────────────────────

$("btn-host").addEventListener("click", async () => {
  localStorage.setItem("zz-name", nameInput.value.trim());
  if (await ensureConnected()) socket.send({ type: "create_lobby", env: "urban" });
});

$("btn-join").addEventListener("click", async () => {
  const code = codeInput.value.trim().toUpperCase();
  if (!code) return fail("enter a lobby code");
  localStorage.setItem("zz-name", nameInput.value.trim());
  if (await ensureConnected()) socket.send({ type: "join_lobby", code });
});

$("btn-start").addEventListener("click", () => socket.send({ type: "start_game" }));
$("btn-leave").addEventListener("click", () => {
  socket.send({ type: "leave_lobby" });
  show(lobbyPanel, false);
  show(menu, true);
});
$("btn-again").addEventListener("click", () => {
  teardownMatch();
  show(lobbyPanel, true);
});

// ── server messages ──────────────────────────────────────────────────────────

function onMessage(msg: ServerMessage): void {
  switch (msg.type) {
    case "welcome":
      myId = msg.player_id;
      break;

    case "lobby_state": {
      inLobbyCode = msg.code;
      hostId = msg.host_id;
      $("lobby-code").textContent = msg.code;
      const roster = $("roster");
      roster.innerHTML = "";
      for (const p of msg.players) {
        const li = document.createElement("li");
        li.textContent = p.name + (p.id === myId ? "  (you)" : "");
        roster.appendChild(li);
      }
      for (let i = msg.players.length; i < 5; i++) {
        const li = document.createElement("li");
        li.className = "open";
        li.textContent = "— open —";
        roster.appendChild(li);
      }
      show($("btn-start"), hostId === myId);
      show(menu, false);
      // The end screen owns the view until the player dismisses it — the
      // post-match lobby_state must not clobber it.
      if (overrun.classList.contains("hidden")) show(lobbyPanel, true);
      break;
    }

    case "game_start": {
      show(lobbyPanel, false);
      show(menu, false);
      show(overrun, false);
      show(hud, true);
      startMatch(msg.map_seed, msg.your_slot, msg.players);
      setBanner("SURVIVE", 2000);
      break;
    }

    case "wave_start":
      setBanner(`WAVE ${msg.wave}`);
      break;

    case "wave_clear":
      setBanner(
        msg.bonus_ammo > 0 ? `WAVE ${msg.wave} CLEAR — +${msg.bonus_ammo} AMMO` : `WAVE ${msg.wave} CLEAR`,
      );
      break;

    case "match_end": {
      const s = msg.stats;
      $("ov-line").textContent =
        `waves cleared ${s.waves_cleared}  ·  ${s.zombies_killed} kills  ·  ` +
        `peak horde ${s.peak_zombies}  ·  ${Math.round(s.duration_ms / 1000)}s`;
      const rows = $("ov-players");
      rows.innerHTML = "";
      for (const p of s.players) {
        const li = document.createElement("li");
        li.textContent = `${p.name} — ${p.kills} kills · ${p.damage_dealt} dmg`;
        rows.appendChild(li);
      }
      game?.stop();
      show(overrun, true);
      break;
    }

    case "ping":
      socket.send({ type: "pong", t: msg.t });
      break;

    case "player_left":
      break; // roster refresh arrives as lobby_state

    case "error":
      fail(msg.message);
      break;
  }
}

function startMatch(seed: string, mySlot: number, roster: RosterPlayer[]): void {
  game?.dispose();
  game = new ZzGame(canvas, socket, seed, mySlot, roster);
  socket.onSnapshot = (snap) => game?.applySnapshot(snap);
  game.onSelfState = (p) => {
    $("zz-health").textContent = String(p.health);
    ($("zz-healthbar") as HTMLElement).style.width = `${Math.max(0, Math.min(100, p.health))}%`;
    $("zz-ammo").textContent = `${p.ammoMag} / ${p.ammoReserve}`;
    $("zz-kills").textContent = String(p.kills);
  };
  game.start();
}
