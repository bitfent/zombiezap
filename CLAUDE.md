# ZombieZap — agent operating manual

You are continuing an in-flight build. **Read `docs/NEXT.md` first** — it is
the authoritative work queue with specs and definition-of-done per item.
The README's Roadmap section mirrors it at lower resolution.

## What this is

Browser co-op zombie survival FPS, 1–5 players, accountless, no database.
100% Rust: `zz-core` (shared deterministic sim — the ONLY implementation of
movement/mapgen/protocol/codec), `zz-server` (tokio+axum, 30 TPS rooms,
in-memory lobbies), `zz-client` (Bevy 0.19, native for dev + wasm via Trunk).
Deploy target: **zombiezap.com**, single Render service (`render.yaml`).

## Commands

```bash
cargo test --workspace                 # must be green before every commit
cargo clippy --workspace --all-targets # ZERO warnings (CI is -D warnings)
cargo run -p zz-server                 # :8080; MAP_SEED / ZZ_DIRECTOR_RATE / PUBLIC_URL env
cargo run -p zz-client                 # native window; ZZ_SERVER / ZZ_NAME / ZZ_JOIN env
cd web && trunk build --release        # wasm → web/dist (server serves it at /)
```

## Binding rules (violations get reverted)

1. Simulation logic exists ONLY in zz-core; server and client both call it.
   `cargo build -p zz-core --target wasm32-unknown-unknown` must always pass.
2. Zero asset files: procedural textures, synthesized audio, generated
   meshes. Exceptions: two pixel fonts, OG image, favicons.
3. Server = plain structs and loops. No ECS/actor/physics frameworks there.
4. Nothing may block a room tick.
5. `legacy/` is the ShotAnte TS original — reference + golden-fixture source.
   The zz-core goldens (`crates/zz-core/tests/goldens.rs`) are bit-exact
   against it; do not break them. Delete legacy/ only when docs/NEXT.md says.
6. Quality bar (user's words): PS2-level graphics OK, but everything must
   read as what it is — zombies look like zombies, weapons look and sound
   like weapons, each map reads as its environment. Mobile and desktop are
   BOTH first-class.

## Dispatching grok build workers (`~/.grok/bin/grok`)

`~/.grok/bin/grok --prompt-file <brief.md> --worktree=<name> --always-approve
--check --max-turns 120 --output-format plain`

- **grok "worktrees" share this checkout.** Commit EVERYTHING before
  dispatching; never edit the tree while a worker runs; run workers
  SEQUENTIALLY (they stash any dirty files they find, including each
  other's). After each worker: check `git stash list`, reconcile, verify.
- Briefs must be exact: allowed-file list, seam types fixed by you, tests to
  write, commit message, definition of done. See docs/NEXT.md for per-task
  briefs. Workers have live web search — use them for post-cutoff APIs
  (Bevy 0.19 notes live in `crates/zz-client/README.md`).

## Git / deploy facts

- Work lands on branch `rust-rewrite`; PR #1 → main on
  github.com/bitfent/zombiezap. Push needs auth the USER provides (gh login
  or a token they paste) — the keychain's default GitHub identity is a
  different account (syscarbonio) and cannot push here. Never commit tokens.
- Render: blueprint in `render.yaml`; env `PUBLIC_URL=https://zombiezap.com`.
  Known caveat: Trunk's wasm-opt needs binaryen in the build image.
- Verify like this session did: bot suites for netcode, and a REAL browser
  session (serve web/dist via zz-server, load it, click through
  host→lobby→match) for anything touching the client.
