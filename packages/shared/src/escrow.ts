// Escrow wiring shared by client and server: the minimal MatchEscrow ABI,
// chain ids, and the wager config shape. token === NATIVE means ETH antes;
// any other address is an ERC-20 (USDC on Base).

export const NATIVE = "0x0000000000000000000000000000000000000000";

export const CHAIN_IDS = {
  base: 8453,
  baseSepolia: 84532,
  foundry: 31337, // local anvil — mainnet rehearsal only
} as const;
export type ChainName = keyof typeof CHAIN_IDS;

// Canonical USDC deployments (constructor params elsewhere; never hardcoded in
// the contract itself). No canonical USDC on anvil — deploy a mock and pass
// its address explicitly if you need the ERC-20 path locally.
export const USDC_ADDRESS: Record<ChainName, string> = {
  base: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913",
  baseSepolia: "0x036CbD53842c5426634e7929541eC2318f3dCF7e",
  foundry: NATIVE,
};

export interface WagerTerms {
  matchIdHex: string; // bytes32 the contract + the game share
  chainId: number;
  escrowAddress: string;
  token: string; // NATIVE or ERC-20 address
  wagerWei: string; // per-player ante, smallest unit, decimal string
  feeBps: number; // protocol fee on the pot, basis points (snapshotted on-chain at create)
  usdCents: number; // the USD tier this ante represents (for stable display)
}

export const ESCROW_ABI = [
  {
    type: "function",
    name: "createMatch",
    stateMutability: "payable",
    inputs: [
      { name: "matchId", type: "bytes32" },
      { name: "token", type: "address" },
      { name: "wager", type: "uint96" },
    ],
    outputs: [],
  },
  {
    type: "function",
    name: "joinMatch",
    stateMutability: "payable",
    inputs: [{ name: "matchId", type: "bytes32" }],
    outputs: [],
  },
  {
    type: "function",
    name: "cancelMatch",
    stateMutability: "nonpayable",
    inputs: [{ name: "matchId", type: "bytes32" }],
    outputs: [],
  },
  {
    type: "function",
    name: "submitResult",
    stateMutability: "nonpayable",
    inputs: [
      { name: "matchId", type: "bytes32" },
      { name: "winner", type: "address" },
      { name: "sig", type: "bytes" },
    ],
    outputs: [],
  },
  {
    // Server-relayed auto-payout: same signed authority as submitResult, but
    // pushes the pot straight to the winner (no claim tx needed).
    type: "function",
    name: "settle",
    stateMutability: "nonpayable",
    inputs: [
      { name: "matchId", type: "bytes32" },
      { name: "winner", type: "address" },
      { name: "sig", type: "bytes" },
    ],
    outputs: [],
  },
  {
    type: "function",
    name: "refund",
    stateMutability: "nonpayable",
    inputs: [{ name: "matchId", type: "bytes32" }],
    outputs: [],
  },
  {
    type: "function",
    name: "claimPayout",
    stateMutability: "nonpayable",
    inputs: [{ name: "token", type: "address" }],
    outputs: [],
  },
  {
    type: "function",
    name: "matches",
    stateMutability: "view",
    inputs: [{ name: "", type: "bytes32" }],
    outputs: [
      { name: "p1", type: "address" },
      { name: "p2", type: "address" },
      { name: "token", type: "address" },
      { name: "wager", type: "uint96" },
      { name: "status", type: "uint8" },
      { name: "winner", type: "address" },
      { name: "createdAt", type: "uint64" },
      { name: "feeBpsSnap", type: "uint16" },
    ],
  },
  {
    type: "function",
    name: "feeBps",
    stateMutability: "view",
    inputs: [],
    outputs: [{ name: "", type: "uint16" }],
  },
  {
    type: "function",
    name: "feeRecipient",
    stateMutability: "view",
    inputs: [],
    outputs: [{ name: "", type: "address" }],
  },
  {
    type: "function",
    name: "claimable",
    stateMutability: "view",
    inputs: [
      { name: "", type: "address" },
      { name: "", type: "address" },
    ],
    outputs: [{ name: "", type: "uint256" }],
  },
] as const;

// ERC-20 approve/allowance — needed for USDC antes.
export const ERC20_ABI = [
  {
    type: "function",
    name: "approve",
    stateMutability: "nonpayable",
    inputs: [
      { name: "spender", type: "address" },
      { name: "value", type: "uint256" },
    ],
    outputs: [{ name: "", type: "bool" }],
  },
  {
    type: "function",
    name: "allowance",
    stateMutability: "view",
    inputs: [
      { name: "owner", type: "address" },
      { name: "spender", type: "address" },
    ],
    outputs: [{ name: "", type: "uint256" }],
  },
] as const;

// Match.status enum values (mirror of the contract)
export const ESCROW_STATUS = {
  None: 0,
  Created: 1,
  Locked: 2,
  Settled: 3,
  Cancelled: 4,
  Voided: 5,
} as const;
