// Fixed gameplay rules (spec §7). Tunable later; fixed now so client prediction
// and server simulation can never disagree on the numbers.

export const TICK_RATE = 30; // server simulation ticks/sec
export const TICK_DT = 1 / TICK_RATE;
export const SNAPSHOT_EVERY_TICKS = 1; // -> 30 snapshots/sec (cheap now: delta frames are ~49B)

// Client renders opponents this far in the past (interpolation buffer). The
// server uses the SAME value for lag compensation, so both agree on "when" the
// shooter saw the world. Shared so they can't drift.
export const INTERP_DELAY_MS = 100;
// Lag comp rewinds a target's position to (now - interp - shooter's one-way RTT),
// but never further than this — caps how stale a hit can be (fairness + abuse
// guard) and bounds the server-side position history we keep.
export const MAX_REWIND_MS = 250;
export const PING_INTERVAL_MS = 2000; // RTT + heartbeat cadence per connection
// No traffic from a client for this long = they left (closed tab / backgrounded /
// dropped). The server terminates the socket so a live match forfeits to whoever
// stayed, instead of hanging until the (minutes-long) TCP timeout. An active
// client sends inputs ~30×/s and pongs every 2s, so this only trips on real absence.
export const PRESENCE_TIMEOUT_MS = 8000;
// When a player drops mid-match the duel PAUSES (clock + sim frozen) and they get
// this long to reconnect and re-enter; if they don't, the match forfeits to whoever
// stayed. One grace per player per match — a second drop forfeits immediately.
export const RECONNECT_GRACE_MS = 20_000;

export const PLAYER_SPEED = 6; // m/s
export const PLAYER_RADIUS = 0.45;
export const PLAYER_EYE = 1.55; // eye height above feet
export const GRAVITY = 20;
export const JUMP_VELOCITY = 7;
export const STEP_UP = 0.55; // auto-climb height — stairs/curbs you walk up

export const MAX_HEALTH = 100;
export const DAMAGE_PER_HIT = 25; // 4 hits to kill
export const FIRE_COOLDOWN_MS = 250; // 4 shots/sec
export const SHOT_RANGE = 70; // covers cross-map shots on the 60x60 arena

// Hitbox = stacked spheres approximating the visible avatar (head at ~1.6,
// torso at ~0.9). One chest sphere made head/leg shots whiff even when they
// looked on target — the hitbox must match what the player SEES.
export const PLAYER_HITBOX: { y: number; r: number }[] = [
  { y: 0.55, r: 0.5 }, // legs/hips
  { y: 1.0, r: 0.55 }, // torso
  { y: 1.6, r: 0.4 }, // head
];

// Health pickups (duels): grab to heal — adds map control like the classics.
export const PICKUP_HEAL = 50;
export const PICKUP_RESPAWN_MS = 15_000;
export const PICKUP_RADIUS = 0.9;

// Explosive barrels: shoot one and it detonates — radius damage with linear
// falloff (3D distance), chains to other barrels, hurts BOTH players.
export const BARREL_RADIUS_DMG = 4;
export const BARREL_DMG_MAX = 75; // at the barrel
export const BARREL_DMG_MIN = 25; // at the edge of the radius
export const BARREL_SPAWN_CLEAR = 6; // never generate a barrel this close to a spawn

export const KILLS_TO_WIN = 5;
export const MATCH_DURATION_MS = 2 * 60_000;
export const SUDDEN_DEATH_MAX_MS = 60_000; // still tied after this = draw (void)
export const RESPAWN_MS = 2_000;
export const COUNTDOWN_MS = 3_000;

export const MAX_INPUTS_PER_SECOND = 90; // anti-cheat: input flood cap
export const MAX_PITCH = Math.PI / 2 - 0.01;

// Chat (ephemeral, relayed over the game WebSocket — never persisted).
// NOTE: in-match proximity chat is now VOICE (see below). The text `chat`
// message is still used for LOBBY chat only.
export const CHAT_MAX_LEN = 120;
export const CHAT_RATE_MS = 1000; // min interval between messages per player
// You only hear the opponent when you are CLOSE — get in their face to talk
// trash. ~one third of the 60m arena width. Used by both lobby-era text chat
// and the new voice proximity gate.
export const CHAT_PROXIMITY_RADIUS = 20;

// Voice proximity chat: hands-free open mic, raw PCM relayed over the game
// WebSocket as binary frames, gated by CHAT_PROXIMITY_RADIUS. 16 kHz mono is
// plenty for speech and keeps bandwidth ~32 KB/s while in range.
export const VOICE_SAMPLE_RATE = 16_000;
export const VOICE_FRAME_MS = 120; // ~1920 samples / 3840 bytes per frame
// Reject oversized binary frames server-side (anti-abuse). A 120ms 16k frame is
// ~3840 bytes; allow generous headroom for jitter/batching.
export const VOICE_MAX_FRAME_BYTES = 16_384;

// Challenges (persistent lobbies). A DeathMatch is a short-lived, transient
// invite: it self-expires so an unanswered link never lingers or surprises you.
export const SCHEDULE_GRACE_MS = 5 * 60_000; // no-show window after scheduled time
export const CHALLENGE_TTL_MS = 45 * 60_000; // a play-now DeathMatch lives 45 min
export const SCHEDULE_MAX_AHEAD_MS = 7 * 24 * 60 * 60_000; // schedule up to a week out

export const PROTOCOL_VERSION = 4;

// Fixed wager tiers, in USD cents. The host picks one of these + a token (USDC
// or Base ETH); USDC maps directly, ETH is converted at match start (price.ts).
export const WAGER_TIERS_USD_CENTS = [25, 100, 500, 1000] as const; // $0.25, $1, $5, $10
export type WagerTokenKind = "usdc" | "eth";
/** Human label for a tier, e.g. 25 -> "$0.25", 100 -> "$1". */
export function wagerTierLabel(cents: number): string {
  return cents % 100 === 0 ? `$${cents / 100}` : `$${(cents / 100).toFixed(2)}`;
}
