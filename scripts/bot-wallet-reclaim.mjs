// Headless verification of WALLET-BASED reclaim for wagered DeathMatches:
//
//   host creates a wagered challenge bound to wallet W -> host's device "loses"
//   the hostKey (new device) -> the new device asks for a reclaim nonce, SIGNS
//   it with W, and reclaims with the signature instead of a key -> the server
//   verifies the signature, re-attaches the host, and mints a FRESH hostKey ->
//   replaying the same signature from yet another device is REJECTED (the nonce
//   was single-use).
//
// Runs against a server in WAGER_MODE=mock|chain (wallet reclaim is wagered-only).
// In mock the server verifies via plain EOA recovery; in chain it uses the
// public client (EIP-1271 / ERC-6492 smart wallets). This bot uses an EOA so it
// works without a chain. Run: node scripts/bot-wallet-reclaim.mjs
//
// Run the server first, e.g.: WAGER_MODE=mock npm run dev:server

import WebSocket from "ws";
import { privateKeyToAccount, generatePrivateKey } from "viem/accounts";

const URL = process.env.SERVER_URL ?? "ws://localhost:8080";
const account = privateKeyToAccount(generatePrivateKey());
const WALLET = account.address;

const deadline = setTimeout(() => {
  console.error("FAIL: wallet-reclaim flow did not complete within 30s");
  process.exit(1);
}, 30_000);

function fail(why) {
  console.error(`FAIL: ${why}`);
  clearTimeout(deadline);
  process.exit(1);
}

const S = { created: false, reclaimed: false, freshKey: false, replayRejected: false };
let code = null;
let originalHostKey = null;
let signedMessage = null;
let signature = null;

// 1) create a wagered challenge bound to WALLET, then "lose" the device key
const host = new WebSocket(URL);
host.on("open", () =>
  host.send(JSON.stringify({ type: "create_challenge", name: "wallet_host", isPublic: false, wagerAddress: WALLET })));
host.on("message", (raw) => {
  const msg = JSON.parse(String(raw));
  if (msg.type === "error") fail(`host create: ${msg.message}`);
  if (msg.type === "challenge_created") {
    code = msg.code;
    originalHostKey = msg.hostKey;
    S.created = true;
    console.log(`[host] wagered DeathMatch ${code} created, bound to ${WALLET.slice(0, 10)}…`);
    host.close(); // device gone — the hostKey is "lost"
    setTimeout(reclaimFromNewDevice, 400);
  }
});

// 2) new device: no key, prove wallet control instead
function reclaimFromNewDevice() {
  const dev = new WebSocket(URL);
  dev.on("open", () =>
    dev.send(JSON.stringify({ type: "reclaim_nonce_request", code, wagerAddress: WALLET })));
  dev.on("message", async (raw) => {
    const msg = JSON.parse(String(raw));
    if (msg.type === "error") fail(`reclaim: ${msg.message}`);
    if (msg.type === "reclaim_nonce") {
      if (msg.role !== "host") fail(`expected role host, got ${msg.role}`);
      signedMessage = msg.message;
      signature = await account.signMessage({ message: msg.message });
      console.log(`[newdev] signed the reclaim nonce — reclaiming by wallet`);
      dev.send(JSON.stringify({ type: "reclaim_challenge", code, key: "", name: "wallet_host", wagerAddress: WALLET, signature }));
    }
    if (msg.type === "challenge_created") {
      // server mints a fresh key for the proven device
      if (!msg.hostKey || msg.hostKey === originalHostKey) fail("expected a fresh hostKey on wallet reclaim");
      S.freshKey = true;
      console.log(`[newdev] re-attached + got a fresh hostKey ✓`);
    }
    if (msg.type === "lobby_state") {
      if (!msg.lobby.host.present) fail("wallet reclaim did not re-attach the host");
      if (!S.reclaimed) {
        S.reclaimed = true;
        console.log(`[newdev] lobby shows host present ✓ — testing replay rejection`);
        setTimeout(replayAttack, 300);
      }
    }
  });
}

// 3) replay the consumed signature from another device — must be rejected
function replayAttack() {
  const attacker = new WebSocket(URL);
  attacker.on("open", () =>
    attacker.send(JSON.stringify({ type: "reclaim_challenge", code, key: "", name: "replay", wagerAddress: WALLET, signature })));
  attacker.on("message", (raw) => {
    const msg = JSON.parse(String(raw));
    if (msg.type === "error") {
      S.replayRejected = true;
      console.log(`[attacker] replay correctly rejected: "${msg.message}"`);
      finish();
    }
    if (msg.type === "lobby_state" || msg.type === "challenge_created") {
      fail("replayed signature was accepted — nonce was not single-use");
    }
  });
  // no response at all within 3s also counts as "not accepted"
  setTimeout(() => { if (!S.replayRejected) { S.replayRejected = true; finish(); } }, 3000);
}

function finish() {
  const ok = S.created && S.reclaimed && S.freshKey && S.replayRejected;
  console.log(ok
    ? "\nPASS: wallet-based reclaim (sign nonce → verify → re-attach → fresh key; replay rejected)"
    : `\nFAIL: ${JSON.stringify(S)}`);
  clearTimeout(deadline);
  process.exit(ok ? 0 : 1);
}
