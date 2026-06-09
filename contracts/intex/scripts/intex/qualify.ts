// Mark a series as Qualified on IntexNFT1155.
//
// Qualification is a discrete state transition (Issued → Qualified). Once a series is
// Qualified, holders are allowed to bridge their Intex to Outbe via the user-driven
// ONFT1155Adapter and to settle voluntarily.
//
// Caller requirements:
//   - The signer must hold `RELAYER_ROLE` on the target IntexNFT1155 contract.
//
// Typical use:
//   - Operator (or Telosis automation) determines the series has met qualification
//     conditions and calls this helper to flip the on-chain state.

import type { Address, Hex } from "viem";

const INTEX_QUALIFY_ABI = [
  {
    inputs: [{ name: "seriesId", type: "uint32" }],
    name: "markQualified",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
  {
    inputs: [{ name: "seriesId", type: "uint32" }],
    name: "readData",
    outputs: [
      {
        components: [
          { name: "intexSize", type: "uint128" },
          { name: "intexStrikePrice", type: "uint64" },
          { name: "coenPriceFloor", type: "uint64" },
          { name: "issuedAt", type: "uint32" },
          { name: "calledAt", type: "uint32" },
          { name: "intexCallPeriod", type: "uint32" },
          { name: "totalSupply", type: "uint32" },
          { name: "settlementTokenAlias", type: "uint16" },
          { name: "status", type: "uint8" },
          { name: "state", type: "uint8" },
          {
            name: "intexCallTrigger",
            type: "tuple",
            components: [
              { name: "windowDays", type: "uint16" },
              { name: "thresholdDays", type: "uint16" },
              { name: "coenPriceCallTrigger", type: "uint64" },
            ],
          },
        ],
        type: "tuple",
      },
    ],
    stateMutability: "view",
    type: "function",
  },
] as const;

const STATE_NAMES = ["Issued", "Qualified", "Called"] as const;

export interface QualifyArgs {
  intexAddress: Address;
  seriesId: number;
}

export interface QualifyClient {
  publicClient: {
    readContract<T = unknown>(args: {
      address: Address;
      abi: readonly unknown[];
      functionName: string;
      args?: unknown[];
    }): Promise<T>;
    waitForTransactionReceipt: (args: { hash: Hex }) => Promise<unknown>;
  };
  walletClient: {
    writeContract<T = Hex>(args: {
      address: Address;
      abi: readonly unknown[];
      functionName: string;
      args: unknown[];
      account: { address: Address };
    }): Promise<T>;
    account: { address: Address };
  };
}

export async function markSeriesQualified(client: QualifyClient, args: QualifyArgs): Promise<Hex> {
  const data = await client.publicClient.readContract<{
    state: number;
  }>({
    address: args.intexAddress,
    abi: INTEX_QUALIFY_ABI,
    functionName: "readData",
    args: [args.seriesId],
  });

  const stateName = STATE_NAMES[data.state] ?? `Unknown(${data.state})`;
  console.log(`[qualify] series ${args.seriesId} current state: ${stateName}`);

  if (data.state !== 0) {
    throw new Error(
      `Series is not in Issued state (${stateName}). markQualified is only valid from Issued.`,
    );
  }

  const tx = await client.walletClient.writeContract<Hex>({
    address: args.intexAddress,
    abi: INTEX_QUALIFY_ABI,
    functionName: "markQualified",
    args: [args.seriesId],
    account: client.walletClient.account,
  });
  await client.publicClient.waitForTransactionReceipt({ hash: tx });
  console.log(`[qualify] markQualified tx: ${tx}`);
  return tx;
}
