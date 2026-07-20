// Mainnet dress rehearsal on a LOCAL chain — proves the entire money path
// with zero risk before any real deployment:
//
//   anvil (chain id 31337)
//     -> deploy MatchEscrow (feeBps=250) + allow-list ETH antes
//     -> game server in WAGER_MODE=chain + RELAYER_PRIVATE_KEY pointed at it
//     -> two bots with REAL anvil wallets play a wagered duel
//        (createMatch / joinMatch txs, server polls the chain until Locked)
//     -> SERVER relays settle(): the pot is PUSHED to the winner's wallet
//        automatically — the winner signs nothing and pays no gas
//     -> assert: winner's wallet grew by exactly pot-minus-fee, no claimable
//     -> assert: fee accrued on-chain to the feeRecipient
//     -> scripts/claim-fees.mjs withdraws it; assert recipient got paid
//
// Run: npm run rehearse:anvil   (needs anvil on PATH; compiles if needed)

import { spawn, execFileSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import WebSocket from "ws";
import { createPublicClient, createWalletClient, http, formatEther } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { foundry } from "viem/chains";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const RPC = "http://127.0.0.1:8546"; // off the default port to avoid collisions
const PORT = 8093;
const WS_URL = `ws://localhost:${PORT}`;
const WAGER = 500_000_000_000_000n; // 0.0005 ETH ante (the launch cap)
const FEE_BPS = 250n;
const NATIVE = "0x0000000000000000000000000000000000000000";

// anvil's deterministic test accounts ("test test … junk")
const KEYS = {
  deployer: "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80",
  settlement: "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b78690d",
  feeRecipient: "0x5de4111afa1a4b94908f83103eb1f1706367c2e68ca870fc3fb9a804cdab365a",
  playerA: "0x7c852118294e51e653712a81e05800f419141751be58f605c371e15141b007a6",
  playerB: "0x47e179ec197488593b187f80a00eb0da91f1b9d0b13f8733639f19c30a34926a",
  relayer: "0x8b3a350cf5c34c9194ca85829a2df0ec3153be0318b5e2d3348e872092edffba",
};

const artifactPath = join(ROOT, "contracts/out/MatchEscrow.json");
if (!existsSync(artifactPath)) {
  console.log("artifact missing — compiling…");
  execFileSync("node", [join(ROOT, "scripts/compile-contract.mjs")], { stdio: "inherit" });
}
const { abi, bytecode } = JSON.parse(readFileSync(artifactPath, "utf8"));

const children = [];
function cleanup(code) {
  for (const c of children) {
    // children are spawned detached => kill the whole process group, otherwise
    // wrappers (npx/tsx) die but the actual server survives as an orphan
    try {
      process.kill(-c.pid, "SIGKILL");
    } catch {
      try {
        c.kill("SIGKILL");
      } catch {
        /* already dead */
      }
    }
  }
  process.exit(code);
}
process.on("SIGINT", () => cleanup(130));
process.on("unhandledRejection", (err) => {
  console.error(`FAIL: ${err instanceof Error ? err.message : err}`);
  cleanup(1);
});
const deadline = setTimeout(() => {
  console.error("FAIL: rehearsal did not complete within 240s");
  cleanup(1);
}, 240_000);

function fail(msg) {
  console.error(`FAIL: ${msg}`);
  cleanup(1);
}

function assert(cond, label) {
  if (!cond) fail(label);
  console.log(`  ✓ ${label}`);
}

// ── 1. anvil ─────────────────────────────────────────────────────────────────
console.log("[1/6] starting anvil…");
const anvil = spawn("anvil", ["--port", "8546", "--silent"], { stdio: "ignore", detached: true });
children.push(anvil);

const pub = createPublicClient({ chain: foundry, transport: http(RPC) });
for (let i = 0; ; i++) {
  try {
    await pub.getChainId();
    break;
  } catch {
    if (i > 50) fail("anvil did not come up");
    await new Promise((r) => setTimeout(r, 200));
  }
}

const wallet = (key) =>
  createWalletClient({ account: privateKeyToAccount(key), chain: foundry, transport: http(RPC) });
const deployer = wallet(KEYS.deployer);
const settlementAddr = privateKeyToAccount(KEYS.settlement).address;
const feeRecipientAddr = privateKeyToAccount(KEYS.feeRecipient).address;
const relayerAddr = privateKeyToAccount(KEYS.relayer).address;

// ── 2. deploy + configure ───────────────────────────────────────────────────
console.log("[2/6] deploying MatchEscrow (feeBps=250)…");
const deployHash = await deployer.deployContract({
  abi,
  bytecode,
  args: [settlementAddr, feeRecipientAddr, Number(FEE_BPS)],
});
const { contractAddress: escrow } = await pub.waitForTransactionReceipt({ hash: deployHash });
const capTx = await deployer.writeContract({
  address: escrow,
  abi,
  functionName: "setMaxWager",
  args: [NATIVE, WAGER * 2n],
});
await pub.waitForTransactionReceipt({ hash: capTx });
console.log(`  escrow at ${escrow}`);

// ── 3. game server in chain mode ────────────────────────────────────────────
console.log("[3/6] starting game server (WAGER_MODE=chain CHAIN=anvil + relayer)…");
const server = spawn(join(ROOT, "node_modules/.bin/tsx"), ["src/index.ts"], {
  cwd: join(ROOT, "apps/server"),
  stdio: ["ignore", "inherit", "inherit"],
  detached: true,
  env: {
    ...process.env,
    PORT: String(PORT),
    WAGER_MODE: "chain",
    CHAIN: "anvil",
    RPC_URL: RPC,
    ESCROW_ADDRESS: escrow,
    SETTLEMENT_PRIVATE_KEY: KEYS.settlement,
    RELAYER_PRIVATE_KEY: KEYS.relayer, // auto-payout: server submits settle()
    WAGER_TOKEN: "eth",
    WAGER_AMOUNT: WAGER.toString(),
    DATABASE_URL: "", // in-memory is fine for the rehearsal
  },
});
children.push(server);

let health = null;
for (let i = 0; ; i++) {
  try {
    health = await fetch(`http://localhost:${PORT}/health`).then((r) => r.json());
    break;
  } catch {
    if (i > 100) fail("game server did not come up");
    await new Promise((r) => setTimeout(r, 300));
  }
}
assert(health.wagerMode === "chain", "server is in chain mode");
assert(health.feeBps === Number(FEE_BPS), `/health reports on-chain feeBps=${health.feeBps}`);
assert(
  health.settlementSigner.toLowerCase() === settlementAddr.toLowerCase(),
  "settlement signer matches the contract's resultSigner",
);
assert(
  (health.relayer ?? "").toLowerCase() === relayerAddr.toLowerCase(),
  "auto-payout relayer is configured",
);

// ── 4. wagered duel with real on-chain antes ────────────────────────────────
console.log("[4/6] playing a wagered duel (real createMatch/joinMatch txs)…");

const readClaimable = (who) =>
  pub.readContract({ address: escrow, abi, functionName: "claimable", args: [who, NATIVE] });

const duelDone = new Promise((resolve, reject) => {
  function bot(name, key, aggressive) {
    const account = privateKeyToAccount(key);
    const w = wallet(key);
    const ws = new WebSocket(WS_URL);
    const state = { id: null, seq: 0 };
    ws.on("error", (e) => reject(new Error(`${name} ws error: ${e.message}`)));
    ws.on("open", () =>
      ws.send(JSON.stringify({ type: "queue", name, wagerAddress: account.address })),
    );
    ws.on("message", async (raw) => {
      const msg = JSON.parse(String(raw));
      try {
        if (msg.type === "welcome") state.id = msg.playerId;

        if (msg.type === "escrow_action") {
          const t = msg.terms;
          console.log(`  [${name}] escrow_action=${msg.action} ante=${formatEther(BigInt(t.wagerWei))} ETH feeBps=${t.feeBps}`);
          if (t.feeBps !== Number(FEE_BPS)) throw new Error(`WagerTerms.feeBps=${t.feeBps}, want ${FEE_BPS}`);
          const hash =
            msg.action === "create"
              ? await w.writeContract({
                  address: t.escrowAddress, abi, functionName: "createMatch",
                  args: [t.matchIdHex, t.token, BigInt(t.wagerWei)], value: BigInt(t.wagerWei),
                })
              : await w.writeContract({
                  address: t.escrowAddress, abi, functionName: "joinMatch",
                  args: [t.matchIdHex], value: BigInt(t.wagerWei),
                });
          await pub.waitForTransactionReceipt({ hash });
          // baseline AFTER the ante: the winner sends no further tx in the
          // auto-payout flow, so any later balance change is purely the payout.
          state.balAfterAnte = await pub.getBalance({ address: account.address });
          console.log(`  [${name}] ${msg.action === "create" ? "createMatch" : "joinMatch"} mined`);
        }

        if (msg.type === "escrow_status" && msg.status === "failed")
          throw new Error(`escrow failed: ${msg.detail}`);
        if (msg.type === "escrow_status" && msg.status === "locked")
          console.log(`  [${name}] escrow locked on-chain ✓`);

        if (msg.type === "snapshot" && aggressive) {
          const me = msg.snapshot.players.find((p) => p.id === state.id);
          const foe = msg.snapshot.players.find((p) => p.id !== state.id);
          if (!me || !foe || !me.alive || !foe.alive) return;
          const dx = foe.x - me.x, dy = foe.y + 1.0 - (me.y + 1.55), dz = foe.z - me.z;
          const far = Math.hypot(dx, dz) > 4;
          const st = state;
          const nowMs = Date.now();
          st.samples = (st.samples || []).filter((q) => nowMs - q.t < 1200);
          st.samples.push({ t: nowMs, x: me.x, z: me.z });
          const oldest = st.samples[0];
          const movedSq = (me.x - oldest.x) ** 2 + (me.z - oldest.z) ** 2;
          if (far && nowMs - oldest.t > 900 && movedSq < 0.09 && nowMs > (st.escapeUntil || 0)) {
            st.escapeUntil = nowMs + 1500;
            st.escapeDir = (st.escapeDir || 1) * -1;
            st.samples = [];
          }
          const escaping = nowMs < (st.escapeUntil || 0);
          ws.send(JSON.stringify({
            type: "input",
            input: {
              sequence: state.seq++, forward: far && !escaping, backward: false,
              left: escaping && st.escapeDir < 0, right: escaping && st.escapeDir > 0,
              jump: false, shoot: true,
              yaw: Math.atan2(-dx, -dz), pitch: Math.atan2(dy, Math.hypot(dx, dz)),
            },
          }));
        }

        if (msg.type === "match_end" && aggressive) {
          const s = msg.settlement;
          if (!s) throw new Error("match_end has no settlement");
          if (s.winnerAddress.toLowerCase() !== account.address.toLowerCase())
            throw new Error("aggressive bot did not win — rerun");
          // NEW auto-payout path: the SERVER relayed settle(), pushing the pot
          // straight to the winner. The winner does nothing — no tx, no gas.
          if (s.payout !== "sent")
            throw new Error(`expected server-relayed payout=sent, got ${s.payout ?? "none"}`);
          const pot = WAGER * 2n;
          const fee = (pot * FEE_BPS) / 10_000n;
          // the only balance change since the ante is the pushed payout
          const balNow = await pub.getBalance({ address: account.address });
          const delta = balNow - (state.balAfterAnte ?? balNow);
          if (delta !== pot - fee)
            throw new Error(`winner wallet grew by ${formatEther(delta)} ETH, want ${formatEther(pot - fee)}`);
          // paid directly — nothing left to pull
          if ((await readClaimable(account.address)) !== 0n)
            throw new Error("winner has a leftover claimable — push payout did not happen");
          console.log(`  [${name}] won — server pushed ${formatEther(pot - fee)} ETH straight to wallet (no claim tx, no gas)`);
          resolve({ pot, fee });
        }
      } catch (err) {
        reject(err);
      }
    });
  }

  bot("rehearsal_alice", KEYS.playerA, true);
  setTimeout(() => bot("rehearsal_bob", KEYS.playerB, false), 300);
});

const { pot, fee } = await duelDone.catch((err) => fail(err.message));

// ── 5. fee accrued on-chain ─────────────────────────────────────────────────
console.log("[5/6] checking the protocol fee…");
const accrued = await readClaimable(feeRecipientAddr);
assert(accrued === fee, `fee accrued to feeRecipient: ${formatEther(fee)} ETH (2.5% of ${formatEther(pot)} pot)`);

// ── 6. claim-fees.mjs withdraws it ──────────────────────────────────────────
console.log("[6/6] withdrawing via scripts/claim-fees.mjs…");
// After settle() pushed the winner their share, the escrow holds exactly the
// fee — the recipient's wallet delta is fee minus gas (negative at this tiny
// ante on anvil's gas price), so the meaningful check is that the ESCROW paid
// the fee out.
const escrowBalBefore = await pub.getBalance({ address: escrow });
execFileSync("node", [join(ROOT, "scripts/claim-fees.mjs")], {
  stdio: "inherit",
  env: {
    ...process.env,
    CHAIN: "anvil",
    RPC_URL: RPC,
    ESCROW_ADDRESS: escrow,
    FEE_RECIPIENT_KEY: KEYS.feeRecipient,
    USDC: "none",
  },
});
assert((await readClaimable(feeRecipientAddr)) === 0n, "claimable drained to zero");
const escrowBalAfter = await pub.getBalance({ address: escrow });
assert(
  escrowBalBefore - escrowBalAfter === fee,
  `escrow paid the ${formatEther(fee)} ETH fee out to the recipient`,
);
assert(escrowBalAfter === 0n, "escrow fully drained — no funds stranded");

clearTimeout(deadline);
console.log("\nPASS: mainnet rehearsal — deploy → wagered duel with real antes → server relayed settle() → pot auto-pushed to the winner (no claim tx) → fee accrued → claim-fees.mjs withdrew it. Same sequence, same scripts on Base.");
cleanup(0);
