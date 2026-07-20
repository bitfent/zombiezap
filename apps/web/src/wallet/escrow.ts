// Client-side escrow transactions (viem + the Base Smart Wallet EIP-1193
// provider). The SERVER dictates the terms (escrow address, chain, token,
// amount) in the escrow_action message; this module just executes them: switch
// chain, ante in (ETH value or USDC approve+transferFrom), and after a win,
// submit the signed result + pull the pot.

import {
  createPublicClient,
  createWalletClient,
  custom,
  encodeFunctionData,
  http,
  numberToHex,
  type Address,
  type Hex,
} from "viem";
import { base, baseSepolia } from "viem/chains";
import { ERC20_ABI, ESCROW_ABI, NATIVE, type WagerTerms } from "@shotante/shared";
import { providerOrThrow } from "./provider.ts";

// Paymaster proxy (server-side, keeps the CDP key secret). When set AND the
// wallet advertises the paymasterService capability, ante/claim gas is
// sponsored — a fresh passkey wallet with zero ETH can still play and claim.
const env = (import.meta as any).env ?? {};
const PAYMASTER_URL: string | null = env.VITE_PAYMASTER_URL ?? null;

function chainFor(chainId: number) {
  if (chainId === base.id) return base;
  if (chainId === baseSepolia.id) return baseSepolia;
  throw new Error(`unsupported chain ${chainId}`);
}

// ── EIP-5792 batched + ERC-7677 sponsored execution ───────────────────────
// Smart wallets (Base Account) can run several calls as one user operation and
// have the gas paid by a paymaster. We detect support per chain and fall back
// to plain sequential transactions for injected/EOA wallets that lack it.

interface Call {
  to: Address;
  data: Hex;
  value?: bigint;
}

type Caps = { atomic: boolean; paymaster: boolean } | null;

async function getCaps(account: Address, chainIdHex: Hex): Promise<Caps> {
  try {
    const provider = providerOrThrow();
    const caps = (await provider.request({
      method: "wallet_getCapabilities",
      params: [account],
    })) as Record<string, any>;
    const c = caps?.[chainIdHex] ?? {};
    return {
      atomic: c.atomic?.supported === "supported" || c.atomic?.status === "supported",
      paymaster: !!c.paymasterService?.supported,
    };
  } catch {
    return null; // wallet doesn't speak EIP-5792 — use the legacy path
  }
}

/** Poll wallet_getCallsStatus until the batch lands; throw if any call reverts. */
async function waitForCalls(id: unknown): Promise<void> {
  const provider = providerOrThrow();
  for (let i = 0; i < 80; i++) {
    const r = (await provider.request({
      method: "wallet_getCallsStatus",
      params: [id],
    })) as any;
    const receipts = r?.receipts;
    if (Array.isArray(receipts) && receipts.length > 0) {
      for (const rc of receipts) {
        if (rc.status === "0x0" || rc.status === 0) throw new Error("sponsored call reverted");
      }
      return;
    }
    if (r?.status === "FAILED" || r?.status === 400 || r?.status === 500) {
      throw new Error("sponsored transaction failed");
    }
    await new Promise((res) => setTimeout(res, 1500));
  }
  throw new Error("timed out waiting for transaction to confirm");
}

/**
 * Execute one or more calls. Uses wallet_sendCalls (batched, optionally
 * gas-sponsored) when the wallet supports it; otherwise sends each call as a
 * plain transaction in order. `sponsor` only takes effect with a configured
 * paymaster and a capable wallet — it never blocks the unsponsored path.
 */
async function execCalls(terms: WagerTerms, account: Address, calls: Call[], sponsor: boolean): Promise<void> {
  const provider = providerOrThrow();
  const chainIdHex = numberToHex(terms.chainId);
  const caps = await getCaps(account, chainIdHex);

  if (caps) {
    const params: Record<string, unknown> = {
      version: "1.0",
      chainId: chainIdHex,
      from: account,
      calls: calls.map((c) => ({
        to: c.to,
        data: c.data,
        value: c.value !== undefined ? numberToHex(c.value) : "0x0",
      })),
    };
    if (caps.atomic && calls.length > 1) params.atomicRequired = true;
    if (sponsor && PAYMASTER_URL && caps.paymaster) {
      params.capabilities = { paymasterService: { url: PAYMASTER_URL } };
    }
    const res = (await provider.request({ method: "wallet_sendCalls", params: [params] })) as any;
    await waitForCalls(typeof res === "string" ? res : res?.id);
    return;
  }

  // Legacy wallets: one transaction per call, awaited in sequence.
  const { wallet, pub, chain } = clients(terms.chainId);
  for (const c of calls) {
    const hash = await wallet.sendTransaction({
      account,
      chain,
      to: c.to,
      data: c.data,
      value: c.value ?? 0n,
    });
    await pub.waitForTransactionReceipt({ hash });
  }
}

function clients(chainId: number) {
  const provider = providerOrThrow();
  const chain = chainFor(chainId);
  return {
    chain,
    wallet: createWalletClient({ chain, transport: custom(provider) }),
    pub: createPublicClient({ chain, transport: http() }),
  };
}

/** Nudge the wallet onto the right chain. Base Account is multi-chain (it
 *  serves any chain in appChainIds), so a switch may be a no-op — never let it
 *  block the ante. */
export async function ensureChain(chainId: number): Promise<void> {
  const provider = providerOrThrow();
  const chain = chainFor(chainId);
  const hex = `0x${chainId.toString(16)}`;
  try {
    await provider.request({
      method: "wallet_switchEthereumChain",
      params: [{ chainId: hex }],
    });
  } catch (e: any) {
    if (e?.code !== 4902) return; // unsupported/no-op on smart wallet — proceed
    try {
      await provider.request({
        method: "wallet_addEthereumChain",
        params: [
          {
            chainId: hex,
            chainName: chain.name,
            nativeCurrency: chain.nativeCurrency,
            rpcUrls: chain.rpcUrls.default.http,
            blockExplorerUrls: [chain.blockExplorers?.default.url],
          },
        ],
      });
    } catch {
      /* couldn't add — the write below targets the chain explicitly anyway */
    }
  }
}

async function anteTx(
  terms: WagerTerms,
  account: Address,
  fn: "createMatch" | "joinMatch",
): Promise<void> {
  await ensureChain(terms.chainId);
  const escrow = terms.escrowAddress as Address;
  const wager = BigInt(terms.wagerWei);
  const native = terms.token === NATIVE;
  const calls: Call[] = [];

  // ERC-20 (USDC) antes need an allowance first; ETH antes just send value.
  // Batched with the ante below, approve + join become a single confirmation.
  if (!native) {
    const { pub } = clients(terms.chainId);
    const allowance = (await pub.readContract({
      address: terms.token as Address,
      abi: ERC20_ABI,
      functionName: "allowance",
      args: [account, escrow],
    })) as bigint;
    if (allowance < wager) {
      calls.push({
        to: terms.token as Address,
        data: encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [escrow, wager] }),
      });
    }
  }

  calls.push({
    to: escrow,
    data: encodeFunctionData({
      abi: ESCROW_ABI,
      functionName: fn,
      args: fn === "createMatch" ? [terms.matchIdHex as Hex, terms.token as Address, wager] : [terms.matchIdHex as Hex],
    }),
    value: native ? wager : undefined,
  });

  await execCalls(terms, account, calls, true);
}

export const sendCreate = (t: WagerTerms, a: Address) => anteTx(t, a, "createMatch");
export const sendJoin = (t: WagerTerms, a: Address) => anteTx(t, a, "joinMatch");

/** Winner path: relay the server-signed result and pull the pot. Batched into
 *  one (gas-sponsored) confirmation when the wallet supports it, so a winner
 *  with no ETH can still claim. */
export async function submitAndClaim(
  terms: WagerTerms,
  winner: Address,
  signature: Hex,
  onStatus: (s: string) => void,
): Promise<void> {
  await ensureChain(terms.chainId);
  const escrow = terms.escrowAddress as Address;

  onStatus("submitting result + claiming the pot…");
  await execCalls(
    terms,
    winner,
    [
      {
        to: escrow,
        data: encodeFunctionData({
          abi: ESCROW_ABI,
          functionName: "submitResult",
          args: [terms.matchIdHex as Hex, winner, signature],
        }),
      },
      {
        to: escrow,
        data: encodeFunctionData({ abi: ESCROW_ABI, functionName: "claimPayout", args: [terms.token as Address] }),
      },
    ],
    true,
  );
  onStatus("pot claimed ✓");
}

/** Recovery paths if a match never starts/settles. */
export async function cancelEscrow(terms: WagerTerms, account: Address): Promise<void> {
  await ensureChain(terms.chainId);
  await execCalls(
    terms,
    account,
    [
      {
        to: terms.escrowAddress as Address,
        data: encodeFunctionData({ abi: ESCROW_ABI, functionName: "cancelMatch", args: [terms.matchIdHex as Hex] }),
      },
    ],
    true,
  );
}
