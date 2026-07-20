// USD -> on-chain amount conversion for the fixed wager tiers.
//
// USDC is a USD stablecoin (6 decimals on Base), so a USD tier maps directly.
// ETH antes need a live price: we fetch ETH/USD from CoinGecko (COINGECKO_API_KEY,
// a CG- demo key -> x-cg-demo-api-key header + the public api host), cache it
// briefly, and lock the converted wei into the WagerTerms at match start so BOTH
// players ante the identical amount.

const COINGECKO_API_KEY = process.env.COINGECKO_API_KEY ?? "";
const PRICE_TTL_MS = 60_000; // re-fetch at most once a minute
const USDC_DECIMALS = 6;
const ETH_DECIMALS = 18;

let cache: { usd: number; at: number } = { usd: 0, at: 0 };

/** Live ETH price in USD (cached ~60s). Throws if it can't be fetched and there's
 *  no usable cached value — callers must NOT guess a price for real money. */
export async function ethUsd(): Promise<number> {
  if (cache.usd > 0 && Date.now() - cache.at < PRICE_TTL_MS) return cache.usd;
  const url = "https://api.coingecko.com/api/v3/simple/price?ids=ethereum&vs_currencies=usd";
  const headers: Record<string, string> = { accept: "application/json" };
  if (COINGECKO_API_KEY) headers["x-cg-demo-api-key"] = COINGECKO_API_KEY;
  const res = await fetch(url, { headers });
  if (!res.ok) {
    if (cache.usd > 0) return cache.usd; // stale-but-better-than-nothing on a transient blip
    throw new Error(`coingecko ${res.status}`);
  }
  const data = (await res.json()) as { ethereum?: { usd?: number } };
  const usd = data?.ethereum?.usd;
  if (!usd || usd <= 0) {
    if (cache.usd > 0) return cache.usd;
    throw new Error("coingecko: no ETH/USD price");
  }
  cache = { usd, at: Date.now() };
  return usd;
}

/** USDC smallest-unit amount for a USD-cents tier (e.g. 100c -> 1_000_000). */
export function usdCentsToUsdcUnits(cents: number): string {
  // cents * 10^(decimals-2)
  return (BigInt(Math.round(cents)) * 10n ** BigInt(USDC_DECIMALS - 2)).toString();
}

/** ETH wei for a USD-cents tier, at the live ETH price. Async (fetches/caches). */
export async function usdCentsToEthWei(cents: number): Promise<string> {
  const price = await ethUsd();
  const eth = cents / 100 / price; // USD / (USD per ETH)
  // these tiers are tiny (≤ ~0.005 ETH), so eth*1e18 stays well under 2^53 — exact
  return BigInt(Math.round(eth * 10 ** ETH_DECIMALS)).toString();
}
