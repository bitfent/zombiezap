# ShotAnte

Retro 3D browser duels with a wagered pot: **connect → ante → fight → win → settle**.

A 1v1 arena shooter that runs in the browser (TypeScript + Three.js), with a
server-authoritative TypeScript game server and (Phase 4) Solidity escrow on
Base. Gameplay is off-chain; the chain only holds and releases money.

## Status

| Phase (per spec) | State |
|---|---|
| 1 — Offline prototype (arena, movement, shooting, targets, HUD) | ✅ playable (PRACTICE RANGE) |
| 2 — Multiplayer (WS, server-authoritative sim, hitscan, score, timer) | ✅ playable (FIND 1v1 DUEL) |
| 3 — Match system (lifecycle, queue, lobby) | ◐ quick-match queues, player-created DeathMatches (public board + private invite links `?join=CODE`, one-at-a-time Home card, 45-min self-expiry, back/cancel + prompt-to-start, wallet-based reclaim), forfeit/void; scheduling temporarily off; replay log pending |
| 4 — Blockchain (escrow, signed settlement) | ✅ **live on Base mainnet**: dual-asset escrow (ETH or USDC antes), fixed USD wager tiers, server escrow watcher + EIP-191 signing + relayer auto-payout. `MatchEscrow` deployed & Basescan-verified at [`0x1356…1281`](https://basescan.org/address/0x1356898d8f171373075aa01cd504604a255e1281). **Unaudited — beta caps (0.01 ETH / 10 USDC)** |
| 5 — Closed beta | ◐ deploying for real-money testing |

## Run it

```bash
npm install

# terminal 1 — game server (ws://localhost:8080)
npm run dev:server

# terminal 2 — web client (http://localhost:5174)
npm run dev:web
```

- **PRACTICE RANGE** works offline (no server): WASD + mouse, shoot the red
  dummies. `[` / `]` adjust mouse sensitivity (persisted).
- **Mobile works** (touch web, no install): left-half virtual stick to move,
  right-half drag to aim, FIRE/JUMP buttons. Landscape recommended. Test on
  desktop with `?touch=1`.
- **QUICK 1v1** needs the server: open the page in two windows, queue in
  both → 3s countdown → first to 5 kills in 2 minutes, sudden death on tie,
  disconnect = forfeit. The server emits a `MatchResult` with a canonical
  `resultHash` — the value the escrow contract settles against.
- **START A DEATHMATCH / BROWSE OPEN DEATHMATCHES**: host a public DeathMatch
  (listed live on the board with its pot) or a private one and share the invite
  link (`/?join=CODE`, QR, or native share sheet); free or wagered (wagered
  needs a connected wallet on both sides). A DeathMatch is a single, transient
  invite: there's **one at a time**, it lives on a Home card with a countdown,
  and it **self-expires after 45 min** so an unanswered link never lingers.
  DeathMatches are persistent within that window — close the tab and come back,
  the link still works. From the lobby, **← BACK** minimizes (keeps it alive,
  shows it on Home) while **CANCEL** kills it; if you've stepped away, a friend
  opening the link is **held** and you get a "START" prompt instead of being
  yanked in. Wagered DeathMatches can also be re-joined from a new device by
  **signing with the slot's wallet** (no device key needed). Lobbies have
  ephemeral text chat, and in-match comms are **proximity voice**: get close and
  your mic opens to the opponent, fading out by 20m (never persisted).
  `npm run verify:challenge` proves the whole loop
  headlessly, and `npm run verify:reclaim` proves wallet-based reclaim (sign
  nonce → re-attach → fresh key, replay rejected).

  > Scheduling (pick a time, reserve a slot) is temporarily disabled for
  > simplicity — the protocol fields are kept so it can be re-enabled later.

Headless proof of the full server loop (queue → match → kills → result):

```bash
npm run dev:server   # in one terminal
npm run verify:match # two bots fight; asserts winner + result hash
```

And of the full WAGERED flow (escrow handshake + verified settlement
signature) without touching a chain:

```bash
WAGER_MODE=mock npm run dev:server
npm run verify:wager
```

## Going on-chain (Base Sepolia)

```bash
npm run test:contract                     # Foundry suite: unit + fuzz + invariant
npm run compile:contract                  # forge build -> contracts/out/MatchEscrow.json
npm run rehearse:anvil                    # full dress rehearsal on a local chain:
                                          # deploy, wagered duel, fee accrues, claim
PRIVATE_KEY=0x… RESULT_SIGNER=0x… npm run deploy:escrow   # prints all env vars
# server:  WAGER_MODE=chain ESCROW_ADDRESS=0x… SETTLEMENT_PRIVATE_KEY=0x… npm run dev:server
# antes:   ETH by default; USDC via deploy-time allow-list (USDC=0x… env) —
#          the client handles approve+join automatically per the server's terms
# fees:    feeBps (default 2.5%) is skimmed from each wagered pot, snapshotted
#          per match at create; collect with `npm run claim:fees`. The UI shows
#          the winner's take net of the fee. Free games never touch the chain.
# mainnet: docs/DEPLOYMENT.md — funding + Render env runbook (live on Base).
```

Flow: p1's browser sends `createMatch` (ETH value or USDC approve+transferFrom),
p2 sends `joinMatch`, the server polls until Locked AND the on-chain wallets
match the queued wallets, the duel runs, the server signs
keccak256(matchId, winner), and the winner's browser submits + claims the pot
(pull-based). If anything stalls: creator can `cancelMatch`, both can `refund`
after the timeout.

Typecheck everything: `npm run typecheck`.

## Deploy to Render (two services, manual — no blueprint)

**1 · Game server — Web Service**
- Repo root, runtime Node. Build: `npm install` · Start: `npm --workspace @shotante/server run start`
- Health check path: `/health` (reports `db` + `wagerMode` too)
- Env: `NODE_VERSION=22` · `DATABASE_URL=<supabase pooler url>` ·
  `WAGER_MODE=off` until the escrow is deployed (then `chain` +
  `ESCROW_ADDRESS`/`SETTLEMENT_PRIVATE_KEY`/`CHAIN` from the deploy script)
- Region: **Frankfurt** (near the eu-north-1 DB and EU players — latency is
  gameplay). Plan: **Starter** — the free tier sleeps and a ~50 s cold start
  kills a duel queue. WebSockets work on Render web services out of the box.

**2 · Web client — Static Site**
- Build: `npm install && npm --workspace @shotante/web run build`
- Publish directory: `apps/web/dist`
- Env: `VITE_SERVER_URL=wss://<your-server>.onrender.com` (note `wss://`)
- Point shotante.com at this service. The client is ~131 KB gzipped on first
  load (Three.js included); the viem/escrow chunk (~87 KB) lazy-loads only
  when someone actually wagers.

**Database** — set `DATABASE_URL` and the server creates its own two tables on
boot (`matches` audit log + `players` W/L). No migrations, no ORM. Without the
var it runs purely in memory (local dev). Writes are fire-and-forget — a DB
hiccup can never lag a duel.

### Turning wagers on

The deployed services boot with `WAGER_MODE=off` — all wager code is dormant and
a normal `git push` autodeploys safely. The escrow is **already deployed and
verified on Base mainnet** ([`0x1356…1281`](https://basescan.org/address/0x1356898d8f171373075aa01cd504604a255e1281)),
so turning real wagers on is purely env-driven — **no code change needed**.

Stakes are fixed USD tiers (**$0.25 / $1 / $5 / $10**) in USDC (1:1) or Base ETH
(USD→ETH at match start via CoinGecko). The host picks tier + token per game.

**Full funding + env runbook: [`docs/DEPLOYMENT.md`](docs/DEPLOYMENT.md).** The
short version:

- **Fund two wallets:** the relayer `0x98F7…4089` (gas for auto-payout) and the
  player test wallets (the stakes). The settlement signer and fee recipient
  never need a balance. Keys/mnemonics live in `.wallets/` (gitignored —
  **never committed**).
- **Server env (Render Web Service):** `WAGER_MODE=chain`, `CHAIN=base`,
  `ESCROW_ADDRESS=0x1356…1281`, a **dedicated** `RPC_URL` (not the public Base
  endpoint), `SETTLEMENT_PRIVATE_KEY` + `RELAYER_PRIVATE_KEY` (secrets, from
  `.wallets/`), `COINGECKO_API_KEY`. Optional `FEE_BPS`, `PAYMASTER_RPC_URL`.
- **Client env (Render Static Site):** just `VITE_SERVER_URL=wss://…` — the
  client reads escrow address/chain/amounts from the server, so there are **no
  `VITE_` escrow vars**.
- **Verify:** `GET /health` → `"wagerMode":"chain"`, non-null `"relayer"`,
  `"feeBps":250`. A wagered match then antes on-chain, the server relays
  `settle()`, and the pot is **pushed to the winner automatically** (no claim
  tx, no gas on their side). The 2.5% fee accrues to the fee recipient; withdraw
  with `npm run claim:fees`. Set `WAGER_MODE=off` to turn wagers back off.

> ⚠️ The contract is **UNAUDITED** and keys must **never** be committed (the
> deploy reads them from `.wallets`/env). Keep ante caps low; consider a Base
> Sepolia rehearsal before pointing more funds at it. See "Before real money".

### Bun

The server runs under Bun too (native TS, no tsx): `npm --workspace
@shotante/server run dev:bun`. **Requires Bun ≥ 1.1** — older Bun (≤1.0.x) has
a `ws` compatibility bug where the server never receives client messages
(verified against 1.0.13). At 2 players / 30 TPS the runtime makes no gameplay
difference; Bun's win here is dev experience.

## Layout

```
apps/web        Vite + Three.js client (prediction, interpolation, HUD, sfx)
apps/server     Node + ws authoritative server (30 TPS sim, 15/s snapshots)
packages/shared one source of truth: constants, types, protocol, ARENA
                geometry, movement + raycast math (client prediction and
                server simulation run the SAME functions)
contracts       MatchEscrow.sol — UNAUDITED source, see contracts/README.md
scripts         bot-*.mjs — headless end-to-end verification (match, wager,
                challenge/DeathMatch flow, wallet-based reclaim)
```

## Design notes

- **Server-authoritative everywhere it counts.** The client sends inputs only;
  movement is integrated server-side (input floods can't speed you up), fire
  cooldown and view angles are validated, hitscan rays start from *server*
  positions with wall occlusion. The client's opinion of a hit is never used.
- **Shared simulation = honest prediction.** `stepBody()` and the raycast
  functions live in `packages/shared` and run identically on both sides, so
  the predicted camera matches the authoritative result up to latency.
- **Seeded-random arenas, fair by construction.** Every match generates a
  fresh map from a seed the server picks and ships in `match_start`; client
  and server run the same generator (`packages/shared/src/mapgen.ts`), so the
  rendered map, the predicted collisions, and the authoritative hitscan can
  never disagree. Maps are 180°-symmetric (equal spawns), keep spawn corners
  clear, guarantee >= 1.5m corridors, block direct spawn-to-spawn sight, and
  all cover is taller than the jump apex. `npm run check:mapgen` fuzzes 500
  seeds against those invariants; `MAP_SEED=<x>` pins the map for tests/ops.
  Practice rolls a new map (and target spots) every session.
- **Retro on purpose** (spec §4): 640×360 internal render upscaled with
  `image-rendering: pixelated`, Lambert materials, box arenas, zero asset
  bytes — textures are generated canvases, sfx are WebAudio oscillators, fonts
  are two OFL pixel fonts. Bundle stays small.
- **Settlement seam.** `apps/server/src/settlement/signResult.ts` hashes the
  canonical result today; Phase 4 signs that hash with the authorized key and
  `MatchEscrow.submitResult()` releases the pot against the signature.

## Before real money (read this twice)

1. Deploy + test escrow on **Base Sepolia** with mock USDC; audit before mainnet.
2. **Gambling/skill-gaming law**: wagering on match outcomes is regulated in
   many jurisdictions (incl. Switzerland). Geo-fence and get counsel before
   enabling real wagers anywhere.
3. Anti-cheat is mitigation, not prevention, in a browser: keep wagers small,
   log replays, review suspicious matches manually (spec §13).
