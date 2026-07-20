// Fee operations for MatchEscrow: read the protocol fees accrued to the
// feeRecipient (pull-based `claimable` per token) and optionally withdraw them.
//
// Read-only (anyone can look):
//   ESCROW_ADDRESS=0x... [CHAIN=baseSepolia|base|anvil] [RPC_URL=...] [USDC=0x...] \
//   node scripts/claim-fees.mjs
//
// Claim (needs the fee recipient's key — fees land in its wallet):
//   FEE_RECIPIENT_KEY=0x... ESCROW_ADDRESS=0x... [CHAIN=...] node scripts/claim-fees.mjs
//
// Compile first if contracts/out/MatchEscrow.json is missing: npm run compile:contract

import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { createPublicClient, createWalletClient, formatEther, formatUnits, getAddress, http } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { base, baseSepolia, foundry } from "viem/chains";

const ROOT = join(dirname(fileURLToPath(import.meta.url)), "..");
const { abi } = JSON.parse(readFileSync(join(ROOT, "contracts/out/MatchEscrow.json"), "utf8"));

const CHAINS = { base, baseSepolia, anvil: foundry, foundry };
const chain = CHAINS[process.env.CHAIN ?? "baseSepolia"];
if (!chain) {
  console.error(`unknown CHAIN=${process.env.CHAIN} (use base | baseSepolia | anvil)`);
  process.exit(1);
}
if (!process.env.ESCROW_ADDRESS) {
  console.error("ESCROW_ADDRESS is required");
  process.exit(1);
}
const escrow = getAddress(process.env.ESCROW_ADDRESS);

// Canonical USDC per chain; override or disable with USDC=0x... / USDC=none.
const DEFAULT_USDC = {
  base: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  baseSepolia: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
};
const NATIVE = "0x0000000000000000000000000000000000000000";

const pub = createPublicClient({ chain, transport: http(process.env.RPC_URL) });
const read = (functionName, args = []) =>
  pub.readContract({ address: escrow, abi, functionName, args });

const claimKey = process.env.FEE_RECIPIENT_KEY;
const claimAccount = claimKey ? privateKeyToAccount(claimKey) : null;

const onChainRecipient = await read("feeRecipient");
const feeBps = await read("feeBps");
const recipient = getAddress(process.env.FEE_RECIPIENT ?? onChainRecipient);

console.log(`MatchEscrow ${escrow} on ${chain.name}`);
console.log(`current fee: ${feeBps} bps (${(Number(feeBps) / 100).toFixed(2)}%)`);
console.log(`fee recipient: ${onChainRecipient}${recipient !== getAddress(onChainRecipient) ? ` (querying ${recipient})` : ""}`);

if (claimAccount && getAddress(claimAccount.address) !== recipient) {
  console.error(`FEE_RECIPIENT_KEY is for ${claimAccount.address}, not the fee recipient ${recipient}`);
  process.exit(1);
}

const tokens = [{ label: "ETH", address: NATIVE, decimals: 18 }];
const usdcEnv = process.env.USDC ?? DEFAULT_USDC[process.env.CHAIN ?? "baseSepolia"];
if (usdcEnv && usdcEnv !== "none") {
  tokens.push({ label: "USDC", address: getAddress(usdcEnv), decimals: 6 });
}

const wallet = claimAccount
  ? createWalletClient({ account: claimAccount, chain, transport: http(process.env.RPC_URL) })
  : null;

let totalClaims = 0;
for (const t of tokens) {
  const amount = await read("claimable", [recipient, t.address]);
  const human = t.decimals === 18 ? formatEther(amount) : formatUnits(amount, t.decimals);
  console.log(`accrued ${t.label}: ${human} (${amount} smallest unit)`);
  if (amount === 0n || !wallet) continue;

  const hash = await wallet.writeContract({
    address: escrow,
    abi,
    functionName: "claimPayout",
    args: [t.address],
  });
  const rcpt = await pub.waitForTransactionReceipt({ hash });
  console.log(`  claimed ${human} ${t.label} -> ${recipient} (tx ${hash}, ${rcpt.status})`);
  totalClaims++;
}

if (!wallet) {
  console.log("\nread-only run — set FEE_RECIPIENT_KEY to actually claim");
} else if (totalClaims === 0) {
  console.log("\nnothing to claim");
}
