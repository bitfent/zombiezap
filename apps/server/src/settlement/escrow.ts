// Server-side escrow integration: wager config from env, on-chain match
// watching, and EIP-191 result signing.
//
// Modes (WAGER_MODE):
//   off   — free duels only (default; no chain, no keys)
//   mock  — full wagered HANDSHAKE without a chain: escrow "locks" as soon as
//           both players are told to ante, results are signed with an
//           ephemeral key. For protocol tests and local dev.
//   chain — the real thing against Base/Base Sepolia: polls MatchEscrow until
//           both antes are Locked, signs results with SETTLEMENT_PRIVATE_KEY.
//
// ⚠️ chain mode env:  ESCROW_ADDRESS, SETTLEMENT_PRIVATE_KEY, CHAIN
//    (base|baseSepolia), optional RPC_URL, WAGER_TOKEN (eth|<erc20 addr>),
//    WAGER_AMOUNT (smallest unit). The settlement key only signs results —
//    it never holds player funds (the contract does).

import { randomBytes } from "node:crypto";
import {
  createPublicClient,
  createWalletClient,
  encodePacked,
  http,
  keccak256,
  verifyMessage as verifyMessageEoa,
} from "viem";
import { generatePrivateKey, privateKeyToAccount } from "viem/accounts";
import { base, baseSepolia, foundry } from "viem/chains";
import {
  CHAIN_IDS,
  ESCROW_ABI,
  ESCROW_STATUS,
  NATIVE,
  USDC_ADDRESS,
  type WagerTerms,
  type WagerTokenKind,
} from "@shotante/shared";
import { usdCentsToEthWei, usdCentsToUsdcUnits } from "./price.ts";

import type { Hex } from "viem";

export type WagerMode = "off" | "mock" | "chain";

const MODE = (process.env.WAGER_MODE ?? "off") as WagerMode;
// foundry = local anvil (chain id 31337) — used only for the mainnet rehearsal.
const CHAIN_NAME =
  process.env.CHAIN === "base"
    ? "base"
    : process.env.CHAIN === "anvil" || process.env.CHAIN === "foundry"
      ? "foundry"
      : "baseSepolia";
const ESCROW_ADDRESS = process.env.ESCROW_ADDRESS ?? "0x0000000000000000000000000000000000000001";

const account = (() => {
  if (MODE === "chain") {
    const key = process.env.SETTLEMENT_PRIVATE_KEY;
    if (!key) throw new Error("WAGER_MODE=chain requires SETTLEMENT_PRIVATE_KEY");
    return privateKeyToAccount(key as `0x${string}`);
  }
  // mock/off: ephemeral key so signing still works end-to-end in tests
  return privateKeyToAccount(generatePrivateKey());
})();

const chain = CHAIN_NAME === "base" ? base : CHAIN_NAME === "foundry" ? foundry : baseSepolia;

const publicClient =
  MODE === "chain"
    ? createPublicClient({ chain, transport: http(process.env.RPC_URL) })
    : null;

// Relayer: a gas-funded hot wallet that SUBMITS the signed settlement so the
// winner is paid automatically (no claim tx, no gas on their side). It only
// pays gas + relays an already-signed result — it can never move funds to
// itself, so a compromise is gas-griefing at worst. Separate from the
// settlement SIGNER key on purpose. Unset -> fall back to the client-submitted
// claim flow.
const relayerAccount =
  MODE === "chain" && process.env.RELAYER_PRIVATE_KEY
    ? privateKeyToAccount(process.env.RELAYER_PRIVATE_KEY as Hex)
    : null;

const relayerClient =
  relayerAccount && MODE === "chain"
    ? createWalletClient({ account: relayerAccount, chain, transport: http(process.env.RPC_URL) })
    : null;

export const wagerMode: WagerMode = MODE;
export const settlementAddress = account.address;
export const relayerAddress = relayerAccount?.address ?? null;
export const canRelaySettlement = (): boolean => relayerClient !== null;

// Protocol fee transparency: in chain mode the fee is whatever the contract
// says (read once at boot via initEscrowFee); mock mode mirrors the deploy
// default so the UI flow matches production.
let feeBps = Number(process.env.FEE_BPS ?? 250);

export function escrowFeeBps(): number {
  return MODE === "off" ? 0 : feeBps;
}

/** Chain mode: read the live feeBps from MatchEscrow so /health, WagerTerms
 * and the pot displays always reflect the on-chain truth. */
export async function initEscrowFee(): Promise<void> {
  if (!publicClient) return;
  try {
    feeBps = Number(
      await publicClient.readContract({
        address: ESCROW_ADDRESS as `0x${string}`,
        abi: ESCROW_ABI,
        functionName: "feeBps",
      }),
    );
    console.log(`escrow feeBps (on-chain): ${feeBps}`);
  } catch (err) {
    console.error(`could not read feeBps from ${ESCROW_ADDRESS} — keeping ${feeBps}`, err);
  }
}

/** Build the escrow terms for a fixed USD tier + token. USDC maps directly; ETH
 *  is converted at the LIVE price right now, so the wei is locked for the match
 *  (both players ante the same). Async because the ETH path hits the price feed. */
export async function newWagerTerms(usdCents: number, tokenKind: WagerTokenKind): Promise<WagerTerms> {
  const isEth = tokenKind === "eth";
  const wagerWei = isEth ? await usdCentsToEthWei(usdCents) : usdCentsToUsdcUnits(usdCents);
  return {
    matchIdHex: `0x${randomBytes(32).toString("hex")}`,
    chainId: CHAIN_IDS[CHAIN_NAME],
    escrowAddress: ESCROW_ADDRESS,
    token: isEth ? NATIVE : USDC_ADDRESS[CHAIN_NAME],
    wagerWei,
    feeBps: escrowFeeBps(),
    usdCents,
  };
}

export interface EscrowState {
  status: "none" | "created" | "locked" | "other";
  p1: string;
  p2: string;
}

// Mock escrow walks the real state sequence (none -> created -> locked) so the
// full handshake — including the p2 "join" action — is exercised without a chain.
const mockPolls = new Map<string, number>();

/** Read the match row from the contract (chain mode). */
export async function readEscrow(matchIdHex: string): Promise<EscrowState> {
  if (!publicClient) {
    const n = (mockPolls.get(matchIdHex) ?? 0) + 1;
    mockPolls.set(matchIdHex, n);
    return { status: n === 1 ? "created" : "locked", p1: "", p2: "" };
  }
  const [p1, p2, , , status] = (await publicClient.readContract({
    address: ESCROW_ADDRESS as `0x${string}`,
    abi: ESCROW_ABI,
    functionName: "matches",
    args: [matchIdHex as `0x${string}`],
  })) as readonly [string, string, string, bigint, number, string, bigint, number];
  const named =
    status === ESCROW_STATUS.None
      ? "none"
      : status === ESCROW_STATUS.Created
        ? "created"
        : status === ESCROW_STATUS.Locked
          ? "locked"
          : "other";
  return { status: named, p1: p1.toLowerCase(), p2: p2.toLowerCase() };
}

/** EIP-191 personal_sign over keccak256(matchId, winner) — exactly what
 * MatchEscrow.submitResult()/settle() verify against the resultSigner. */
export async function signSettlement(matchIdHex: string, winner: string): Promise<string> {
  const digest = keccak256(
    encodePacked(["bytes32", "address"], [matchIdHex as `0x${string}`, winner as `0x${string}`]),
  );
  return account.signMessage({ message: { raw: digest } });
}

/**
 * Relay the signed result on-chain via MatchEscrow.settle(): the winner's pot
 * is pushed to them in this transaction, so they need no gas and no claim step.
 * Resolves once the tx is mined; rejects (so the caller can fall back to the
 * client-submitted claim flow) on any failure. No-op-throws if no relayer is
 * configured.
 */
export async function relaySettlement(
  matchIdHex: string,
  winner: string,
  signature: string,
): Promise<string> {
  if (!relayerClient || !publicClient) throw new Error("no relayer configured");
  const hash = await relayerClient.writeContract({
    address: ESCROW_ADDRESS as Hex,
    abi: ESCROW_ABI,
    functionName: "settle",
    args: [matchIdHex as Hex, winner as Hex, signature as Hex],
    chain,
  });
  const receipt = await publicClient.waitForTransactionReceipt({ hash });
  if (receipt.status !== "success") throw new Error("settle tx reverted");
  return hash;
}

/**
 * Verify that `address` signed `message`. In chain mode this goes through the
 * public client, which transparently handles plain EOAs, EIP-1271 smart-contract
 * wallets (Base Smart Wallet is one), and ERC-6492 (not-yet-deployed) signatures.
 * Off-chain (mock/off) it falls back to plain EOA recovery — enough for tests.
 * Used for wallet-based reclaim of wagered DeathMatches.
 */
export async function verifyWalletSignature(
  address: string,
  message: string,
  signature: string,
): Promise<boolean> {
  try {
    if (publicClient) {
      return await publicClient.verifyMessage({
        address: address as Hex,
        message,
        signature: signature as Hex,
      });
    }
    return await verifyMessageEoa({ address: address as Hex, message, signature: signature as Hex });
  } catch {
    return false;
  }
}
