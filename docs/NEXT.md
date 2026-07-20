# NEXT — the authoritative work queue

Ordered. Take the top unchecked item, do it solo (one grok worker OR
yourself, never parallel workers — see CLAUDE.md), verify, commit, push,
check it off, update the README roadmap if the picture changed.

State when this file was written: M0–M6 complete. The game runs in a real
browser end to end (menu → host lobby → code → match on the generated urban
map). All workspace tests green, clippy zero. PR #1 tracks `rust-rewrite`.

---

## [x] 1. Light & atmosphere + retro target (USER-APPROVED PLAN, Phase A+B) — DONE (zz-client M7a + WebGL2 fix)

Browser-verified 2026-07-20: menu → host → lobby → urban match, chunky
480×270 upscale, bright per-env sky/fog, sun-bleached palette, HUD/egui
native-res, letterbox on portrait, 130-300 fps (debug wasm). Critical fix
found in verification: the retro target must be plain Rgba8UnormSrgb with
NO view format — WebGL2 can't reinterpret texture views (README item 35).
Deferred to item 3 (needs `?touch=1` clickable controls for automation):
building-interior close-up screenshot. New: Q/X keyboard turn keys.

User bar, verbatim: "lightweight but nice. It's ok if we have ps2 graphics
but it must look like a game." Real failure observed live: building
interiors and shadowed streets render pitch black; zombies invisible.

Phase A — light (port ShotAnte's recipe; see legacy/apps/web/src/game/Game.ts
buildWorld/loadArena):
- Per-env sky color + distance fog; hemisphere/ambient fill so NOTHING is
  ever pitch black (interiors dim-but-readable); one directional sun.
- Bake shadows ONCE per map load, never per frame (ShotAnte's single biggest
  frame-budget win); re-bake only if geometry changes.
- Tint material families with the map accent; raise entity/world contrast.
- Acceptance: screenshots inside a building and in the darkest street corner
  — everything readable.

Phase B — retro identity target:
- Render 3D at 480x270 (tune vs 640x360), nearest-neighbor upscale to the
  window; MSAA off; HUD/egui at native res on top; optional mild color
  quantization. Chunky pixels make low-poly read as deliberate (PS1/PS2).
- This is also the mobile performance lever.
- Files: crates/zz-client/src/ (new retro.rs; map_render.rs lighting;
  main.rs camera wiring). Bevy 0.19 render-target notes are in
  crates/zz-client/README.md items 14-18.

## [x] 2. REAL zombies + survivor + weapon rigs (Phase C — user priority) — DONE (zz-client M9, commit f9d338b)

Browser-verified 2026-07-20 as far as automation allows: rifle viewmodel
with front sight bottom-right through the retro target; rig cuboids
(head/arms distinct, kind colors) confirmed at point-blank in death
frames; teammate health-bar HUD; 2-player lobby (invite ?join= prefill →
join → roster → match → two-row stats) all work; peak horde 16, ~200 fps
single tab. OUTSTANDING acceptance (blocked on item 3's ?touch=1
clickable controls for automation movement): the mid-range stranger-test
horde screenshot — silhouette variants, telegraph pose, desynced phases,
eye glow. Take it FIRST thing after item 3 lands.

Not placeholders. Zero asset files still — procedural articulated rigs:
- Zombie: head/torso/2 arms/2 legs cuboids on joint pivots; lurching walk
  cycle driven by interpolated movement speed with PER-ZOMBIE phase offset
  (no lockstep horde); arms-raised windup while snapshot state==1 (attack
  telegraph); death crumple on despawn; glowing eyes (emissive) for
  dark-readability; silhouette-distinct kinds: walker (rotten green,
  shamble), runner (gaunt, fast cadence), brute (1.5x bulk, heavy sway).
- Survivor: same rig family, slot-colored, walk gait.
- First-person: boxy rifle viewmodel bottom-right, recoil kick + muzzle
  flash quad on own Shot events (seams::FxQueue already carries them).
- Zombie growls: new Sfx variant + proximity trigger + synth (cap ~6).
- Instancing/shared-mesh pass so 200 rigged zombies stay cheap (ShotAnte's
  own comment warns per-avatar draw calls don't scale — a horde is exactly
  that case).
- Acceptance: the stranger test — one glance at a screenshot says "zombie
  game with a gun"; 60 fps in-browser with 150 zombies on a mid laptop;
  side-by-side screenshot vs ShotAnte for parity.

## [x] 3. Mobile touch experience (task "M6c" part 1) — DONE (zz-client M10, commit cd6e9c8)

Browser-verified 2026-07-20 with ?touch=1: virtual stick + JUMP/NADE/FIRE
cluster render clear of the HUD; aim-drag turns the camera (verified
live); HTML name/code overlays appear in touch mode (soft-keyboard
bridge). REMAINING: verify on an actual phone (soft keyboard, audio
unlock, landscape hint) once deployed — and take item 2's outstanding
stranger-test horde screenshot using these controls.

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

## [x] 3b. Rome EUR real-place map (USER REQUEST 2026-07-20) — DONE (R2a 304c5e2 + R2b 8982846)

Browser-verified live 2026-07-20: ROME EUR in the picker, piazzale spawn
under warm Mediterranean light, real OSM travertine blocks at true 500 m
scale, billboard + cover, OSM footer attribution, rifle hitscan confirmed
(ACC 100% on a fired shot). Playtest findings for item 7:
- Director pacing on the big map IS sparse as flagged: AFK survival 213 s
  vs 18 s urban — zombies walk minutes to cross 250 m. Needs env-aware
  pacing (spawn-at-nearer-gates or per-map director scale).
- FPS slid 286 → ~28 (debug wasm) as the horde hit ~20 on Rome — profile
  rig entity count × big map (instancing pass from item 2 spec).
- Damage vignette washed the whole frame red on a 2 HP graze — review
  vignette intensity curve (likely same overlay implicated in the
  Ended-screen FPS bug).
HUMAN CHECKLIST (automation can't drive touch/aim): basilica interior
walkthrough, horde close-ups, live stick/aim-drag feel, real-phone
soft keyboard + audio unlock.

Viale dei Santi Pietro e Paolo + Via Eufrate at TRUE scale, the piazzale,
and the basilica with a playable interior. Feasibility proven; data cached.

- Source: OSM (ODbL — add "© OpenStreetMap contributors" to page footer).
  Cached corridor extract: `scripts/rome-eur/rome-corridor.json`
  (79 buildings, both streets; basilica way 23432370, height=88).
  Prototype decomposition + preview: `scripts/rome-eur/bake-prototype.py`,
  `arena-preview.svg`.
- Arena: 500×500 m true scale (`arena_half = 250` — fits the i16 wire
  format's ±255.9 m exactly; do NOT touch POS_SCALE, goldens are bit-exact).
  Basilica-anchored window: 96% of both streets fit; the 4 street/edge
  crossings become the gates.
- Baker: port the prototype to a committed generator emitting a Rust
  fixture module (zero-asset rule: generated CODE, not data files at
  runtime); ~770 AABBs after 2 m-grid greedy decomposition.
- Authored on top of the real footprints: basilica interior (hollow
  Greek-cross nave, portal from the piazzale, columns, altar, stacked-box
  dome tiers), street cover (parked-car/planter crates), spawn cluster on
  the piazzale, per-env lighting entry (warm Mediterranean sun).
- KNOWN RISKS: 6× bigger arena than today — director pacing, WalkGrid
  resolution, ground-texture/occlusion-bake resolution all need scaling
  checks; if 5 players feel lost, the baker takes a `--clip 300` cut
  centered on basilica + Via Eufrate crossing.
- DoD: fuzz-grade invariants pass on the fixture (spawns, ≥3 BFS gates);
  60 fps in-browser; the basilica reads instantly in a screenshot.

## [x] 4. Share package (task "M6c" part 2) — DONE (web M8c, commit 306fa64: og.jpg + favicons + meta + Trunk copy directives)

- Generate `web/og.jpg` (1200×630): adapt `legacy/scripts/gen-brand.mjs`
  (node; pixel fonts are in `legacy/scripts/*.ttf`) for ZOMBIEZAP branding —
  zombie-green blocky title on near-black, tagline
  "free co-op zombie survival in your browser". Generate favicon set.
- `web/index.html`: og:title/description/image (ABSOLUTE url on
  https://zombiezap.com), twitter:card=summary_large_image.
- DoD: paste a link into a Discord/Slack preview debugger — real card.
- Later (not now): per-lobby dynamic unfurls served by zz-server when a
  crawler hits `/?join=CODE`.

## [SUPERSEDED — split into items 1-2 above] Visual identity (task #14)

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

## [x] 5. Environments: Mountain Town, Desert Town, Sea Town — zz-core HALF DONE (M8b, commit 1e35c05: generators + 800-map fuzz; mountain flattened to y=0 with terrace character — 2D WalkGrid MAX_FLOOR=1.0). REMAINING → item 5b below.

## [ ] 5b. Per-env client dressing (Phase D: palettes, glowing window slits, lit billboards, clouds) + browser pass on all 4 envs

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

## [ ] 6. Proximity voice (task #13) — server relay DONE (zz-server M8a, commit 9049422: BIN_VOICE=2, authoritative slot rewrite, radius fan-out, bot suite). REMAINING: client capture/playback below.

Design (ShotAnte port, one-to-N): new binary tag BIN_VOICE=2 —
`[tag u8, speaker_slot u8, 16kHz mono i16 PCM ~120ms]`. Server
(`crates/zz-server/src/room/mod.rs`): relay to teammates within
CHAT_PROXIMITY_RADIUS (add const ≈25.0; authoritative positions; size-cap
frames; never persisted). Client: wasm capture via
getUserMedia+AudioWorklet (reuse `legacy/apps/web/public/voice-capture-worklet.js`),
native via cpal; per-speaker jitter buffer, distance fade/pan from
interpolated positions; mute key (M). Server relay is small — do it first
with a bot test (two bots in range → frames relayed; out of range → dropped).

## [ ] 7. Tuning + hardening (task #12, ongoing)

- Difficulty curve at real latency with real players (director constants in
  `crates/zz-core/src/constants.rs`).
- wasm size diet (5.5 MB brotli today): feature audit, `wasm-opt` flags,
  check tonemapping_luts/ktx2 weight.
- Stress: scripted 5 bots + 200 zombies for 10 min; tick p99 < 8 ms.
- BUG (seen in a real run): stats screen showed time_alive 42s > match
  duration 35s, and zombies_killed 7 with player K/DMG/ACC all 0 — audit
  MatchStats accounting in crates/zz-server/src/room/mod.rs finish().
- BUG (browser verification 2026-07-20): the Ended/stats overlay tanks
  client FPS (2-9 fps observed vs 130-300 in-match; also a odd dark
  vertical band on the right of the frozen frame) — profile the Ended
  state (vignette/tint overlay? frozen-world interpolation?).
- Room-task panic guard (catch/log/respawn), per-IP connection caps.
- Then: delete `legacy/` (after #2 no longer needs its scripts), and the
  temporary `#![allow(dead_code)]` in `crates/zz-client/src/seams.rs` should
  be droppable once all consumers exist.

---

Post-v1 parking lot: per-lobby OG unfurls, reconnect-to-match tokens,
lag-compensated hitscan (rewind), interest-culled snapshots, persistence +
accounts, the ad platform behind the billboards.
