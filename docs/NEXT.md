# NEXT — the authoritative work queue

Ordered. Take the top unchecked item, do it solo (one grok worker OR
yourself, never parallel workers — see CLAUDE.md), verify, commit, push,
check it off, update the README roadmap if the picture changed.

State when this file was written: M0–M6 complete. The game runs in a real
browser end to end (menu → host lobby → code → match on the generated urban
map). All workspace tests green, clippy zero. PR #1 tracks `rust-rewrite`.

---

## [ ] 1. Mobile touch experience (task "M6c" part 1)

Port the ShotAnte touch scheme — reference implementation:
`legacy/apps/web/src/game/Input.ts`.

- Left half of screen: virtual stick → forward/backward/left/right.
- Right half: drag = aim (yaw/pitch delta); tap zones don't fire.
- On-screen FIRE / JUMP / GRENADE buttons feeding the same `PlayerInput`
  (30 Hz send cadence in `crates/zz-client/src/game.rs` stays untouched).
- Detect touch capability; `?touch=1` forces it on desktop for testing.
- Audio unlock on first touch (browser autoplay policy).
- HUD (crates/zz-client/src/hud.rs) must not collide with thumbs — check
  bottom corners.
- KNOWN RISK: egui text fields don't summon mobile soft keyboards. Name and
  lobby-code entry on touch devices likely need thin HTML overlay `<input>`s
  in `web/index.html`, bridged into `seams::LobbyView` (platform.rs has the
  wasm side; add a small JS↔wasm bridge or DOM polling).
- DoD: playable on an actual phone against a deployed/LAN server — move,
  aim, shoot, throw, host and join by code. Landscape hint shown in
  portrait.

## [ ] 2. Share package (task "M6c" part 2)

- Generate `web/og.jpg` (1200×630): adapt `legacy/scripts/gen-brand.mjs`
  (node; pixel fonts are in `legacy/scripts/*.ttf`) for ZOMBIEZAP branding —
  zombie-green blocky title on near-black, tagline
  "free co-op zombie survival in your browser". Generate favicon set.
- `web/index.html`: og:title/description/image (ABSOLUTE url on
  https://zombiezap.com), twitter:card=summary_large_image.
- DoD: paste a link into a Discord/Slack preview debugger — real card.
- Later (not now): per-lobby dynamic unfurls served by zz-server when a
  crawler hits `/?join=CODE`.

## [ ] 3. Visual identity (task #14 — USER PRIORITY)

Bar: PS2-level fidelity, but everything must read as what it is. Zero asset
files — procedural meshes/textures only.

- Zombies (`crates/zz-client/src/game.rs` spawns them; consider a new
  `models.rs`): articulated box-humanoids (head/torso/2 arms/2 legs as
  separate cuboids on joint pivots), lurching walk cycle driven by movement
  speed, arms-raised windup when snapshot `state == 1` (attack telegraph),
  death crumple on despawn. Kind variants: walker = rotten green, shambling;
  runner = gaunt, faster cycle; brute = 1.5× bulk, slow heavy sway.
  Zombie growl Sfx: add variant to `seams.rs` Sfx enum + proximity trigger
  in game.rs + synth in audio.rs (cap concurrency).
- Players: same rig family, survivor-colored per slot, subtle walk anim.
- First-person weapon: boxy rifle viewmodel bottom-right of camera, recoil
  kick + muzzle-flash quad on own Shot events (FxQueue already carries them).
- Retro render target: render 3D at 480×270 (tune vs 640×360), nearest
  upscale, HUD at native res. This is also the mobile perf lever.
- DoD: screenshot review — a stranger identifies "zombie game with a gun"
  instantly; native + browser parity; clippy zero; suites green.

## [ ] 4. Environments: Mountain Town, Desert Town, Sea Town (task #11)

In `crates/zz-core/src/map/` (share `primitives.rs`/`builder.rs` machinery;
see `urban.rs`):

- Mountain: 2–3 terrace slabs (y 0/1.5/3.0) joined by wide stair runs,
  cabins (small buildings), gates at low+high edges.
- Desert: low flat-roof adobe (h≈2.6), walled compounds from L-walls, a well
  centerpiece, open sightlines, more street cover.
- Sea: shoreline strip where ground is cosmetic water (knee-high dock edge
  keeps play on land), piers from bridge decks, big two-room warehouses.
- Each env: 5-spawn cluster, ≥3 BFS-reachable gates, billboards — reuse
  `coop.rs`; extend `generate_map` dispatch.
- Per-env material families/palettes in `crates/zz-client/src/map_render.rs`
  — each town recognizable at a glance (user bar).
- Fuzz: extend `crates/zz-core/tests/mapgen_fuzz.rs` to all 4 envs.
- DoD: fuzz green ×4; load each env in browser + native; env picker works.

## [ ] 5. Proximity voice (task #13)

Design (ShotAnte port, one-to-N): new binary tag BIN_VOICE=2 —
`[tag u8, speaker_slot u8, 16kHz mono i16 PCM ~120ms]`. Server
(`crates/zz-server/src/room/mod.rs`): relay to teammates within
CHAT_PROXIMITY_RADIUS (add const ≈25.0; authoritative positions; size-cap
frames; never persisted). Client: wasm capture via
getUserMedia+AudioWorklet (reuse `legacy/apps/web/public/voice-capture-worklet.js`),
native via cpal; per-speaker jitter buffer, distance fade/pan from
interpolated positions; mute key (M). Server relay is small — do it first
with a bot test (two bots in range → frames relayed; out of range → dropped).

## [ ] 6. Tuning + hardening (task #12, ongoing)

- Difficulty curve at real latency with real players (director constants in
  `crates/zz-core/src/constants.rs`).
- wasm size diet (5.5 MB brotli today): feature audit, `wasm-opt` flags,
  check tonemapping_luts/ktx2 weight.
- Stress: scripted 5 bots + 200 zombies for 10 min; tick p99 < 8 ms.
- Room-task panic guard (catch/log/respawn), per-IP connection caps.
- Then: delete `legacy/` (after #2 no longer needs its scripts), and the
  temporary `#![allow(dead_code)]` in `crates/zz-client/src/seams.rs` should
  be droppable once all consumers exist.

---

Post-v1 parking lot: per-lobby OG unfurls, reconnect-to-match tokens,
lag-compensated hitscan (rewind), interest-culled snapshots, persistence +
accounts, the ad platform behind the billboards.
