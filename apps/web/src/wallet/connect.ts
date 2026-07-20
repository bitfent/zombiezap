// Wallet connect. Two entry points share one "active provider":
//   connectWallet()    — Base Account (passkey smart wallet, no extension)
//   connectInjected()  — an injected extension wallet (MetaMask, Rabby, …)
// The escrow phase reuses whichever provider connected for ante/claim txs.

import {
  getProvider,
  setActiveProvider,
  discoverInjectedWallets,
  type Eip1193,
  type InjectedWallet,
} from "./provider.ts";

async function requestAccount(provider: Eip1193): Promise<string | null> {
  const accounts = (await provider.request({ method: "eth_requestAccounts" })) as string[];
  return accounts?.[0] ?? null;
}

/** Connect the Base Account (passkey) wallet. */
export async function connectWallet(): Promise<string | null> {
  try {
    const provider = await getProvider();
    const addr = await requestAccount(provider);
    if (addr) setActiveProvider(provider);
    return addr;
  } catch {
    // user dismissed the passkey popup, or sign-in failed
    return null;
  }
}

/** List injected browser-extension wallets (EIP-6963). */
export function listInjectedWallets(): Promise<InjectedWallet[]> {
  return discoverInjectedWallets();
}

/** Connect a specific injected wallet and make it the active provider. */
export async function connectInjected(w: InjectedWallet): Promise<string | null> {
  try {
    const addr = await requestAccount(w.provider);
    if (addr) setActiveProvider(w.provider);
    return addr;
  } catch {
    // user rejected the connection in the extension
    return null;
  }
}

export function shortAddress(addr: string): string {
  return `${addr.slice(0, 6)}…${addr.slice(-4)}`;
}
