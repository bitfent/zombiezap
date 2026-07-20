// Wallet providers. Two ways to connect:
//   1. Base Account (passkey smart wallet) — the Base Account SDK hands us an
//      EIP-1193 provider that authenticates with a device passkey in a popup,
//      no extension/app needed. Imported dynamically so the heavy SDK lands in
//      its own lazy chunk and stays out of the initial bundle.
//   2. An injected browser-extension wallet (MetaMask, Rabby, …) discovered via
//      EIP-6963. These are plain EOA wallets — escrow.ts already falls back to
//      sequential eth_sendTransaction when a wallet lacks EIP-5792 batching.
//
// Whichever the user connects becomes the single "active" provider that
// connect.ts and escrow.ts (ante/claim) route through, instead of
// window.ethereum.

import { base, baseSepolia } from "viem/chains";

export interface Eip1193 {
  request(args: { method: string; params?: unknown[] }): Promise<unknown>;
}

let baseAccount: Eip1193 | null = null;
let creating: Promise<Eip1193> | null = null;
let active: Eip1193 | null = null;

/** Create (once) and return the Base Account EIP-1193 provider. Does NOT make it
 *  active — connect.ts marks it active only after the user actually connects. */
export async function getProvider(): Promise<Eip1193> {
  if (baseAccount) return baseAccount;
  if (!creating) {
    creating = (async () => {
      const { createBaseAccountSDK } = await import("@base-org/account");
      const sdk = createBaseAccountSDK({
        appName: "ShotAnte",
        appLogoUrl: "https://shotante.com/apple-touch-icon.png",
        appChainIds: [base.id, baseSepolia.id],
      });
      baseAccount = sdk.getProvider() as Eip1193;
      return baseAccount;
    })();
  }
  try {
    return await creating;
  } finally {
    creating = null;
  }
}

/** Mark a provider (Base Account or injected) as the connected one. */
export function setActiveProvider(p: Eip1193): void {
  active = p;
}

/** Synchronous accessor for code paths (escrow) that run only after connect. */
export function providerOrThrow(): Eip1193 {
  if (!active) throw new Error("wallet not connected");
  return active;
}

// ── EIP-6963: discover injected browser-extension wallets ────────────────────

export interface InjectedWallet {
  uuid: string;
  name: string;
  icon: string;
  rdns: string;
  provider: Eip1193;
}

/** Collect every injected wallet that announces itself (EIP-6963). Resolves
 *  after a short window since announcements are async and unbounded. */
export function discoverInjectedWallets(timeoutMs = 350): Promise<InjectedWallet[]> {
  return new Promise((resolve) => {
    const byUuid = new Map<string, InjectedWallet>();
    const onAnnounce = (e: Event) => {
      const d = (e as CustomEvent).detail as { info?: Omit<InjectedWallet, "provider">; provider?: Eip1193 };
      if (d?.info?.uuid && d.provider) byUuid.set(d.info.uuid, { ...d.info, provider: d.provider });
    };
    window.addEventListener("eip6963:announceProvider", onAnnounce as EventListener);
    window.dispatchEvent(new Event("eip6963:requestProvider"));
    setTimeout(() => {
      window.removeEventListener("eip6963:announceProvider", onAnnounce as EventListener);
      resolve([...byUuid.values()]);
    }, timeoutMs);
  });
}
