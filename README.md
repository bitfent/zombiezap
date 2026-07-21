# ZombieZap

**Free co-op zombie survival in your browser.** No account, no install, no
waiting room: host a game, send a friend a five-letter code — or just a link —
and hold out together against a horde that never stops coming.

Up to **5 players** drop into a procedurally generated town with a rifle and
two grenades each. Zombies pour in from the map edges, faster and meaner every
minute, until the team is overrun. Then you look at the stats screen, argue
about whose fault it was, and hit rematch — same lobby, fresh map.

**Stack: 100% Rust.** A Bevy client compiled to WebAssembly, a Tokio
authoritative game server, and one shared simulation crate compiled into
both — so the physics your browser predicts and the physics the server
enforces are, by construction, the same code.

---

## Play

### In the browser (once deployed — zombiezap.com)

Open the page. Type a name. **HOST A GAME** → share the code or the
`?join=CODE` link → **START**. That's the entire onboarding.

### Local dev

```bash
# optional: browser client (Trunk → web/dist), then served by zz-server at /
cargo install trunk --locked
# SHIP path: wasm-release profile + binaryen -Oz (see web/index.html data-wasm-opt)
cd web && trunk build --release --cargo-profile wasm-release && cd ..

# terminal 1 — server on :8080 (healthz, /ws, and static client at /)
cargo run -p zz-server
# open http://localhost:8080  if web/dist exists

# terminal 2..n — native windows (optional; same server)
cargo run -p zz-client
```

Debug `trunk build` is fine for iteration; **ship** with
`trunk build --release --cargo-profile wasm-release` (documented size in
`crates/zz-client/README.md`).

Override the static root with `ZZ_WEB_DIST` (default `web/dist`, relative to
the process cwd). Missing dist does not crash the server — only static routes
404; `/healthz` and `/ws` still work.

**Desktop controls:** WASD move · mouse aim (click to lock) · LMB fire ·
Space jump · G grenade · P pause · Esc release cursor.
**Mobile:** left half of the screen is a virtual stick, right half drags your
aim; FIRE / JUMP / GRENADE buttons on screen.

## The game

- **Endless escalation.** A spawn director accrues a points budget that ramps
  every minute (with deliberate lull/peak waves) and spends it at map gates
  the players can't currently see. Walkers first; Runners join at minute 2,
  Brutes at minute 4.
- **Real aim matters.** Server-side hitscan against body and head spheres —
  headshots double damage, walls actually occlude, tracers show everyone's
  shots.
- **Grenades arc, land, and cook.** Linear-falloff radius damage. Friendly
  fire is off; your *own* grenade still hurts you at half strength, because
  you knew better.
- **Loot keeps you alive.** Kills drop ammo, health, and grenades where the
  zombie fell — wade into the horde's wake to resupply, or starve at range.
- **Any player can pause.** The whole simulation freezes — it's a co-op game
  between friends, not a ranked ladder.
- **Death is spectating; a team wipe is stats.** Kills, damage, accuracy,
  time alive, wave reached — then everyone lands back in the same lobby for
  the rematch.

## Architecture

```
crates/zz-core     the shared simulation: AABB movement with step-up, ray math,
                   seeded procedural map generation, wire protocol, and a
                   binary delta-snapshot codec. No I/O, no engine, no async.
                   Compiled natively into the server AND to wasm32 into the
                   client — prediction and authority cannot drift.
crates/zz-server   tokio + axum. One task per room at 30 TPS (tick-counted
                   time — pause is free), sequence-gated input queues, zombie
                   flow-field AI, spawn director, combat, loot, in-memory
                   lobbies with invite codes. No database, by design.
crates/zz-client   Bevy. Merged-mesh map rendering with procedural textures,
                   client-side prediction with replay reconciliation,
                   100 ms interpolation for remotes, egui lobby, bevy_ui HUD,
                   synthesized audio. Runs native for dev, wasm for players.
web/               Trunk shell: loading screen, OG/share tags, favicon.
legacy/            ShotAnte — the TypeScript 1v1 arena shooter this game was
                   salvaged from. Kept as reference until v1 parity, then gone.
```

### Netcode, in one paragraph

Clients send 14-byte input frames at 30 Hz and predict their own movement by
running the *same* `step_body` the server runs. The server simulates each
accepted input exactly once, acknowledges the last-applied sequence inside
every snapshot, and the client rolls back to server truth and replays what's
still pending — corrections fade through a decaying render offset instead of
snapping. World state ships as binary delta snapshots against the previous
frame (change-masked players at 30 Hz; zombies/loot at 15 Hz with per-entity
id deltas), with periodic keyframes to bound error and resync joiners. Two
hundred moving zombies delta-encode under 2 KB; a busy client sees roughly
20–25 KB/s.

### Determinism, earned the hard way

- All transcendentals in the sim go through `libm` — native and wasm agree
  bit-for-bit.
- The map generator runs in f64 internally and is **golden-tested bit-exact**
  against the original TypeScript generator it was ported from (same seed →
  the same town, byte for byte).
- The RNG is a Mulberry32 port that is bit-compatible with the TS original —
  which is how the goldens exist at all.
- Found along the way: serde_json's default float parsing is 1 ULP off on
  some doubles. The `float_roundtrip` feature is on. You're welcome.

## Testing

```bash
cargo test --workspace            # everything below
cargo test -p zz-core             # sim units + golden tests + codec fuzz
cargo test -p zz-core --test mapgen_fuzz   # 200-seed co-op map invariants
cargo test -p zz-server           # headless BOT suites over the real protocol
```

The bot suites are the ShotAnte inheritance we're proudest of: real
tokio-tungstenite clients speak the real wire protocol at a real server —
lobby create/join/start, movement acks, presence timeouts, horde growth, an
aimbot that must land kills, a team that must get overrun into a coherent
stats screen, and a pause that must freeze every zombie mid-lurch.

## Deployment (Render)

One web service serves everything on the same origin — the Trunk wasm client
at `/`, the WebSocket at `/ws`, health at `/healthz`. Invite links and the
game URL share that origin (`PUBLIC_URL`).

```bash
# render.yaml is in the repo root; Blueprint deploy picks it up.
# Build: wasm32 + trunk release (→ web/dist) + cargo release zz-server.
# Set one env var:
PUBLIC_URL=https://zombiezap.com    # invite-link base AND the game URL
```

Health check: `/healthz`. WebSockets work out of the box; TLS is Render's
problem. Use at least the Starter plan — free-tier cold starts kill lobbies.
Custom domain: add `zombiezap.com` + `www` under Settings → Custom Domains
and set the two DNS records Render shows you.

**Operational honesty:** state lives in memory. A deploy or restart drops
live lobbies and matches. That is a feature until the day it isn't, and that
day is when persistence gets designed on purpose — not before.

## Design pillars

1. **One simulation, two targets.** zz-core is the only implementation of
   movement, maps, and the wire format. There is no second copy to drift.
2. **Box world, zero assets.** Every map is AABBs; every texture is
   generated; every sound is synthesized. The repo ships two pixel fonts and
   an OG image, and that's the entire asset budget.
3. **The server is plain Rust.** Rooms are structs with Vecs, ticked by a
   timer. No ECS, no actor framework, no ORM — nothing between the game rules
   and the code that runs them.
4. **Nothing blocks a tick.** Slow socket? Disconnected. Stats write? Fire
   and forget. The simulation never waits for anyone.
5. **When in doubt, do what ShotAnte did.** This game is built from the bones
   of a 1v1 wager-duel shooter whose README bragged about 131 KB bundles and
   server-authoritative everything. The wagers are gone; the discipline stays.

## Roadmap

### Done

| Milestone | State |
|---|---|
| Shared sim core, golden-exact port from the TS original | ✅ |
| Authoritative server: horde AI, director, combat, loot, stats | ✅ |
| Playable client: prediction, HUD, FX, audio, stats screen | ✅ |
| Lobbies: codes, invite links, host migration, rematch | ✅ |
| Browser build, served by the game server | ✅ verified live: page → lobby → match, in a real browser |

Also banked from that browser session: background tabs used to get
presence-kicked in 8 s (render loop throttles → heartbeat stops); the server
now sends protocol-level pings, which browsers answer even from throttled
tabs. Dead connections still time out.

### Newly landed (this wave)

| Milestone | State |
|---|---|
| Light & atmosphere + 480×270 retro render target: per-env sky/fog/sun, shadows baked once per map, nothing pitch black | ✅ M7a |
| Share package: OG card, favicons, social meta, Trunk copy directives | ✅ M8c |
| Mountain / Desert / Sea Town generators + 800-map 4-env fuzz (zz-core half) | ✅ M8b |
| Proximity-voice server relay: BIN_VOICE frames, authoritative slot, radius fan-out (server half) | ✅ M8a |

### Next, in order

1. **Visual identity.** Zombies become lurching articulated figures with
   kind variants and attack telegraphs, players become survivors, a rifle
   viewmodel kicks and flashes. PS2-level is fine; *reading wrong* is not.
2. **Mobile.** Touch controls (left-half stick, right-half aim drag,
   on-screen FIRE/JUMP/GRENADE — the scheme proven in `legacy/`),
   soft-keyboard-safe name/code entry, audio unlock on first touch. Phones
   are not a port target, they are half the audience.
3. **Rome EUR real-place map.** Viale dei Santi Pietro e Paolo + Via
   Eufrate at true 500 m scale from OpenStreetMap data, the piazzale, and
   the basilica with a playable interior (feasibility proven; corridor
   data + prototype in `scripts/rome-eur/`).
4. **Per-env dressing.** Palettes, glowing window slits, lit billboards —
   each of the four towns recognizable at a glance in the browser.
5. **Proximity voice, client half.** Capture + playback with distance
   fade; the server relay is already live. No WebRTC, nothing stored.
6. **Tuning at real latency.** Director curve, hit feel at 50–100 ms, wasm
   size diet (currently ~5.5 MB brotli), a 5-player-plus-200-zombie stress
   histogram — with real players on the deployed server.

Deploy target: **zombiezap.com** (single Render service, `render.yaml` in
the root; `PUBLIC_URL` is both the game URL and the invite-link base). One
known deploy caveat: Trunk's `wasm-opt` step needs binaryen present in
Render's build image — first deploy log will tell.

Billboards already exist in every map as non-colliding ad surfaces with
placeholder art; the ad platform behind them is deliberately post-v1, as are
accounts and persistence.

---

*Built from [ShotAnte](legacy/)'s bones. Same retro heart, more teeth.*
