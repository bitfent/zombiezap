# ShotAnte contracts

`src/MatchEscrow.sol` — the escrow for wagered 1v1s: two equal antes (ETH or
an allow-listed ERC-20 like USDC), settlement released against a result signed
by the authorized game-server key, refund timeout, pause, per-token beta wager
caps. Two settlement paths share the same signed authority:

- `settle(matchId, winner, sig)` — the game server relays this from a
  gas-funded wallet; the winner's payout is **pushed** to them in the same tx
  (no claim transaction, no gas on the winner's side). If the push fails it
  falls back to the winner's pull balance so funds are never stuck.
- `submitResult(matchId, winner, sig)` — original pull-based path (winner is
  credited, then calls `claimPayout`). Kept as a fallback.

Either way the protocol fee accrues to `feeRecipient` as a pull-based
`claimable` balance.

Protocol fee: `feeBps` (cap 10%) is skimmed from the pot at settlement and
accrues to `feeRecipient` as a pull-based `claimable` balance. The fee is
**snapshotted per match at `createMatch`** — `setFee` only affects future
matches, never funds already in escrow. Ownership transfer is 2-step
(`transferOwnership` + `acceptOwnership`).

**Status: tested with Foundry (unit + fuzz + invariant), NOT audited, NOT on
mainnet.** Free games never touch this contract — chain mode only routes
explicitly wagered games through escrow.

## Build & test

Foundry is the toolchain (`forge-std` is vendored in `lib/`):

```bash
npm run compile:contract   # forge build -> contracts/out/MatchEscrow.json {abi, bytecode}
npm run test:contract      # forge test (lifecycle, sigs, fee math/snapshot, claims,
                           #             reentrancy, pause, ownership, fuzz, invariant)
```

The invariant suite hammers the contract with randomized create/join/settle/
cancel/refund/claim/setFee sequences and asserts the escrow balance always
covers the sum of claimables.

## Fee operations

```bash
# read accrued fees (anyone):
ESCROW_ADDRESS=0x… CHAIN=base node scripts/claim-fees.mjs

# withdraw them to the fee recipient's wallet:
FEE_RECIPIENT_KEY=0x… ESCROW_ADDRESS=0x… CHAIN=base node scripts/claim-fees.mjs
```

Adjust the fee later (owner only, future matches only):
`cast send $ESCROW "setFee(uint16,address)" 250 $TREASURY --private-key $OWNER_KEY`.

## Mainnet runbook (Base)

Deployment stays manual and deliberate. In order — do not skip steps:

1. **Tests green.** `npm run test:contract` and `npm run typecheck` pass.
2. **Local rehearsal.** `npm run rehearse:anvil` — deploys to a local anvil,
   plays a real wagered duel through the server, asserts the fee accrues on
   chain and `claim-fees.mjs` withdraws it.
3. **Base Sepolia dress rehearsal.**
   ```bash
   npm run compile:contract
   PRIVATE_KEY=0x… RESULT_SIGNER=0x… FEE_RECIPIENT=0x… \
     CHAIN=baseSepolia FEE_BPS=250 node scripts/deploy-escrow.mjs
   ```
   Point a staging server at it (`WAGER_MODE=chain CHAIN=baseSepolia …`), play
   wagered duels with test ETH, then claim the test fees:
   `FEE_RECIPIENT_KEY=… ESCROW_ADDRESS=… CHAIN=baseSepolia npm run claim:fees`.
4. **Independent review/audit.** Unaudited code must not hold mainnet funds.
   The test suite is a floor, not a substitute.
5. **Deploy to Base mainnet** with tiny launch caps:
   ```bash
   CHAIN=base FEE_BPS=250 FEE_RECIPIENT=<treasury> RESULT_SIGNER=<settlement addr> \
     PRIVATE_KEY=0x… MAX_WAGER_ETH_WEI=1000000000000000 \
     USDC=0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913 MAX_WAGER_USDC=5000000 \
     node scripts/deploy-escrow.mjs
   ```
6. **Verify on Basescan** so players can read the code they're anteing into:
   ```bash
   forge verify-contract --root contracts --chain base \
     --constructor-args $(cast abi-encode "constructor(address,address,uint16)" \
       $RESULT_SIGNER $FEE_RECIPIENT 250) \
     <deployed address> src/MatchEscrow.sol:MatchEscrow \
     --etherscan-api-key $BASESCAN_API_KEY
   ```
7. **Flip the server env** (`WAGER_MODE=chain CHAIN=base ESCROW_ADDRESS=…
   SETTLEMENT_PRIVATE_KEY=…`) and mirror the `VITE_*` values the deploy script
   prints into the web build. Free games are unaffected — quick 1v1, free
   challenges and practice stay walletless.
8. **Mainnet smoke test:** one real paid duel played solo with two of your own
   wallets in two browser windows at the launch cap (~0.0005 ETH ante) —
   confirm the winner claims the pot and the fee shows up in
   `npm run claim:fees`.
9. **Legal:** wagering on match outcomes is regulated territory. Geo-fence
   first, expand with counsel.

Ops afterwards: `npm run claim:fees` on a schedule, `setMaxWager` to raise the
caps gradually, `setPaused(true)` is the kill switch (players can still refund
and claim while paused — pause blocks entry, never exit).
