# Deployment & funding runbook

How to take ShotAnte live with **real-money wagers on Base mainnet**: which
wallets to fund, which env vars to set on Render, and how to verify it works.

> ⚠️ The `MatchEscrow` contract is **UNAUDITED**. Keep the ante caps low (the
> deploy sets 0.01 ETH / 10 USDC) and treat this as a small beta. Wagering on
> match outcomes is regulated in many jurisdictions — get counsel and geo-fence
> before opening it up.

---

## 1. The deployed contract (Base mainnet)

| | |
|---|---|
| **MatchEscrow** | [`0x1356898d8f171373075aa01cd504604a255e1281`](https://basescan.org/address/0x1356898d8f171373075aa01cd504604a255e1281) — verified |
| Chain | Base mainnet (id `8453`) |
| Settlement signer | `0x96a56dbE2b23870fa38f530F369fa935523f6320` |
| Fee recipient | `0xBC601a7AF94470FD8C40A6F70967Df247891e5d4` (2.5% / 250 bps) |
| ETH antes | enabled, cap **0.01 ETH** |
| USDC antes | `0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913`, cap **10 USDC** |

Stakes are fixed USD tiers — **$0.25 / $1 / $5 / $10** — in USDC (1:1) or Base
ETH (converted from USD at match start via CoinGecko). USDC is always USDC on
Base and ETH is always Base ETH; no other token can be selected.

To re-deploy (e.g. after an audit) see [Re-deploying](#5-re-deploying-the-contract).

---

## 2. Wallets — which to fund, and how much

Four infra wallets exist in `.wallets/` (gitignored — **never committed**). Their
private keys and 12-word mnemonics live there locally; back them up to a password
manager. **Only two ever need a balance.**

| Wallet | Address | Needs funding? | How much (beta) | Why |
|---|---|---|---|---|
| Owner / deployer | `0x8ED15dE9eb01D7f17438Ad4bB473e3262A41fF15` | ✅ gas | ~0.002 ETH (one-time) | deploys the contract + sets caps; already funded & used |
| **Relayer** | `0x98F72c4Ad5E2DE84a96644c00307A54d95914089` | ✅ gas, ongoing | ~0.005 ETH (top up) | pays gas to push each winner's payout (`settle()`) |
| Settlement signer | `0x96a56dbE2b23870fa38f530F369fa935523f6320` | ❌ | — | only **signs** results off-chain; never sends a tx |
| Fee recipient | `0xBC601a7AF94470FD8C40A6F70967Df247891e5d4` | ❌ | — | only **receives** protocol fees |

**Player test wallets** (also in `.wallets/`) carry the actual stakes:

| | Needs | Suggested for testing all tiers |
|---|---|---|
| Each player | ETH for antes + gas, and/or USDC for USDC duels | ~0.02 ETH + ~$25 USDC each |

The relayer is a **hot wallet** on a live server — keep it isolated. It can only
spend its own gas: `settle()` pays the *winner*, never the relayer. Keep the
owner/deployer cold once deployment is done.

---

## 3. Render env vars

Two Render services: a **Web Service** (game server) and a **Static Site** (web
client). Secrets go in the Render dashboard env editor — **never in git**.

### Game server (Web Service)

| Var | Value | Secret? |
|---|---|---|
| `WAGER_MODE` | `chain` | |
| `CHAIN` | `base` | |
| `ESCROW_ADDRESS` | `0x1356898d8f171373075aa01cd504604a255e1281` | |
| `RPC_URL` | **dedicated** Base RPC (Alchemy / Infura / QuickNode) — *not* the public `mainnet.base.org`; it rate-limits a continuously-polling server | |
| `SETTLEMENT_PRIVATE_KEY` | `.wallets/settlement-signer.json` → `privateKey` (its address must equal the signer above) | 🔒 |
| `RELAYER_PRIVATE_KEY` | `.wallets/relayer.json` → `privateKey` (gas-funded; auto-pays winners) | 🔒 |
| `COINGECKO_API_KEY` | `CG-…` (drives USD→ETH conversion for the ETH tiers) | 🔒 |
| `DATABASE_URL` | Supabase/Postgres pooler URL (omit → in-memory only) | 🔒 |
| `NODE_VERSION` | `22` | |
| `FEE_BPS` | optional override; the contract already stores 250 | |
| `PAYMASTER_RPC_URL` | optional — ERC-7677 endpoint to sponsor ante gas so a USDC-only wallet can play with no ETH | 🔒 |

`FEE_RECIPIENT` is **not** a runtime var — it's baked into the contract at deploy
time. The web client gets the escrow address, chain, and amounts from the
server's `escrow_action` message, so there are **no `VITE_` escrow vars**.

### Web client (Static Site)

| Var | Value |
|---|---|
| `VITE_SERVER_URL` | `wss://<your-server>.onrender.com` (note `wss://`) |
| `VITE_SHARE_BASE` | optional — base URL for invite/share links (e.g. `https://shotante.com`) |
| `VITE_PAYMASTER_URL` | optional — only if using sponsored gas |

---

## 4. Verify after deploy

1. `GET https://<server>/health` → `"wagerMode":"chain"`, a non-null
   `"relayer"`, and `"feeBps":250` read straight from the contract.
2. Fund the relayer and the two player wallets (table above).
3. Run a real duel: both players ante on-chain → escrow Locks → duel → the
   server signs `keccak256(matchId, winner)` → the relayer pushes the pot to the
   winner automatically (no claim tx, no gas on the player's side). The 2.5% fee
   accrues to the fee recipient; withdraw anytime with `npm run claim:fees`.

To turn wagers **off** without touching the contract: set `WAGER_MODE=off` and
redeploy. All wager code goes dormant; free duels keep working.

---

## 5. Re-deploying the contract

```bash
npm run test:contract        # Foundry: unit + fuzz + invariant
npm run compile:contract     # forge build -> contracts/out/MatchEscrow.json

# keys/addresses are read from .wallets / .env — nothing secret on the command line
PRIVATE_KEY=<owner-deployer key> \
RESULT_SIGNER=0x96a56dbE2b23870fa38f530F369fa935523f6320 \
FEE_RECIPIENT=0xBC601a7AF94470FD8C40A6F70967Df247891e5d4 \
CHAIN=base \
RPC_URL=<base rpc> \
ETHERSCAN_API_KEY=<basescan key> \
node scripts/deploy-escrow.mjs        # prints the new ESCROW_ADDRESS + env block
```

It deploys, allow-lists ETH (0.01 cap) and USDC (10 cap), and prints a
`forge verify-contract` command for Basescan source verification. Update
`ESCROW_ADDRESS` on Render afterward.

For a **safer path**, rehearse first: `npm run rehearse:anvil` (local chain:
deploy → wagered duel → fee accrues → claim) or deploy with `CHAIN=baseSepolia`
against the free testnet faucet before pointing real funds at it.
