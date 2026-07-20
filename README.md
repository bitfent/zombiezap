# ZombieZap

Browser-based co-op zombie survival FPS for 1–5 players. Survive an endless,
escalating horde on procedurally generated maps. No account needed — open the
page, share a lobby code, play.

**Stack: 100% Rust.**

```
crates/zz-core     shared deterministic sim: movement, raycast, mapgen (4 environments),
                   protocol, binary delta-snapshot codec. Compiled natively (server)
                   AND to wasm32 (client) — prediction and authority run the SAME code.
crates/zz-server   tokio + axum authoritative WebSocket server: lobbies, 30 TPS rooms,
                   zombie AI (flow fields), combat, loot, pause, stats.
crates/zz-client   Bevy first-person client: native for dev, WASM (Trunk) for the browser.
                   Retro low-res look, zero art assets — everything procedural.
web/               Trunk shell for the browser build.
legacy/            the ShotAnte (1v1 arena duel) TypeScript codebase this game was
                   salvaged from. Reference + golden-test source during the port;
                   deleted once the Rust port reaches parity.
```

## Status

Rewrite in progress — see milestones in the project plan. The legacy ShotAnte
game (TypeScript) still lives under `legacy/` and is runnable per its README.

## Design pillars

- **Server-authoritative everywhere it counts** — clients send inputs, the server
  simulates; hits, health, and loot are never client-decided.
- **One sim, two targets** — `zz-core` is the only implementation of the physics,
  map generator, and wire codec; the browser predicts with the exact code the
  server runs.
- **Box world, zero assets** — every map is AABBs + procedural textures +
  synthesized audio; the whole game ships as one wasm bundle and two pixel fonts.
- **Match flow** — create/join a lobby by code or invite link, pick an environment
  (Urban, Mountain Town, Desert Town, Sea Town), survive as long as you can,
  compare stats, run it back.
