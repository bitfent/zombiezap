// Deploy MatchEscrow to Base Sepolia (or Base mainnet) and allow-list antes.
//
//   PRIVATE_KEY=0x... \
//   RESULT_SIGNER=0x... \            (the game server's settlement address)
//   FEE_RECIPIENT=0x... \
//   [CHAIN=baseSepolia|base|anvil] \      (anvil = local rehearsal chain)
//   [FEE_BPS=250] \
//   [MAX_WAGER_ETH_WEI=1000000000000000] \      (0.001 ETH beta cap)
//   [USDC=0x... MAX_WAGER_USDC=5000000] \       (5 USDC beta cap, 6 decimals)
//   node scripts/deploy-escrow.mjs
//
// Compile first: npm run compile:contract. ⚠️ Testnet first. Unaudited.

import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createWalletClient, createPublicClient, http, getAddress } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { base, baseSepolia, foundry } from "viem/chains";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const { abi, bytecode } = JSON.parse(
  readFileSync(join(ROOT, "contracts/out/MatchEscrow.json"), "utf8"),
);

// anvil/foundry (chain id 31337) is for the local mainnet rehearsal only.
const chain =
  process.env.CHAIN === "base"
    ? base
    : process.env.CHAIN === "anvil" || process.env.CHAIN === "foundry"
      ? foundry
      : baseSepolia;
const account = privateKeyToAccount(process.env.PRIVATE_KEY);
const wallet = createWalletClient({ account, chain, transport: http(process.env.RPC_URL) });
const pub = createPublicClient({ chain, transport: http(process.env.RPC_URL) });

const resultSigner = getAddress(process.env.RESULT_SIGNER);
const feeRecipient = getAddress(process.env.FEE_RECIPIENT ?? account.address);
const feeBps = Number(process.env.FEE_BPS ?? 250);

console.log(`deploying MatchEscrow to ${chain.name} from ${account.address}…`);
const hash = await wallet.deployContract({ abi, bytecode, args: [resultSigner, feeRecipient, feeBps] });
const rcpt = await pub.waitForTransactionReceipt({ hash });
const escrow = rcpt.contractAddress;
console.log(`MatchEscrow deployed at ${escrow}`);

const NATIVE = "0x0000000000000000000000000000000000000000";
// Caps must cover the top fixed tier ($10). 0.01 ETH covers $10 down to ETH≈$1000;
// 10 USDC is exactly the $10 tier. Override via env for tighter/looser betas.
const ethCap = BigInt(process.env.MAX_WAGER_ETH_WEI ?? 10_000_000_000_000_000n); // 0.01 ETH
let tx = await wallet.writeContract({ address: escrow, abi, functionName: "setMaxWager", args: [NATIVE, ethCap] });
await pub.waitForTransactionReceipt({ hash: tx });
console.log(`ETH antes enabled, cap ${ethCap} wei (≈0.01 ETH)`);

// Canonical USDC per chain (enabled by default so the USDC tiers work). Pass
// USDC= explicitly for a custom/mock token.
const USDC_DEFAULT = {
  [base.id]: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  [baseSepolia.id]: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
};
const usdcAddr = process.env.USDC ?? USDC_DEFAULT[chain.id] ?? null;
if (usdcAddr) {
  const usdcCap = BigInt(process.env.MAX_WAGER_USDC ?? 10_000_000n); // 10 USDC ($10 tier)
  tx = await wallet.writeContract({ address: escrow, abi, functionName: "setMaxWager", args: [getAddress(usdcAddr), usdcCap] });
  await pub.waitForTransactionReceipt({ hash: tx });
  console.log(`USDC antes enabled (${usdcAddr}), cap ${usdcCap} (10 USDC)`);
}

console.log(`\nserver env:\n  WAGER_MODE=chain\n  ESCROW_ADDRESS=${escrow}\n  CHAIN=${process.env.CHAIN ?? "baseSepolia"}\n  SETTLEMENT_PRIVATE_KEY=<key for ${resultSigner}>\n  RELAYER_PRIVATE_KEY=<gas wallet for auto-payout>\n  RPC_URL=<your Base RPC>\n  COINGECKO_API_KEY=<for ETH tiers>\n(the web client gets escrow address/chain/amount from the server's escrow_action — no VITE_ escrow vars needed)`);

if (process.env.ETHERSCAN_API_KEY) {
  console.log(
    `\nverify on Basescan:\n  forge verify-contract ${escrow} contracts/src/MatchEscrow.sol:MatchEscrow \\\n    --chain-id ${chain.id} --etherscan-api-key $ETHERSCAN_API_KEY \\\n    --constructor-args $(cast abi-encode "constructor(address,address,uint16)" ${resultSigner} ${feeRecipient} ${feeBps})`,
  );
}
