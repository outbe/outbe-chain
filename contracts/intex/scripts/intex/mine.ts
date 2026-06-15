// Mine Promis by atomically burning Settled Intex via IntexSettlement.

import {
  encodePacked,
  keccak256,
  sha256,
  type Address,
  type Hex,
} from "viem";

const SETTLEMENT_ABI = [
  {
    inputs: [
      { name: "seriesId", type: "uint32" },
      { name: "amount", type: "uint256" },
      { name: "nonce", type: "uint256" },
    ],
    name: "minePromis",
    outputs: [{ name: "promisAmount", type: "uint256" }],
    stateMutability: "nonpayable",
    type: "function",
  },
  {
    inputs: [
      { name: "seriesId", type: "uint32" },
      { name: "holder", type: "address" },
    ],
    name: "mineSeq",
    outputs: [{ type: "uint32" }],
    stateMutability: "view",
    type: "function",
  },
] as const;

const PROMIS_BALANCE_ABI = [
  {
    inputs: [{ name: "account", type: "address" }],
    name: "balanceOf",
    outputs: [{ type: "uint256" }],
    stateMutability: "view",
    type: "function",
  },
] as const;

const INTEX_READ_ABI = [
  {
    inputs: [{ name: "seriesId", type: "uint32" }],
    name: "settledTokenId",
    outputs: [{ type: "uint256" }],
    stateMutability: "pure",
    type: "function",
  },
  {
    inputs: [
      { name: "account", type: "address" },
      { name: "id", type: "uint256" },
    ],
    name: "balanceOf",
    outputs: [{ type: "uint256" }],
    stateMutability: "view",
    type: "function",
  },
] as const;

function findPoWNonce(seed: Hex): bigint {
  for (let nonce = 0n; nonce < 1_000_000n; nonce++) {
    const h = sha256(encodePacked(["bytes32", "uint64"], [seed, nonce]));
    if (h.startsWith("0x00")) return nonce;
  }
  throw new Error("PoW nonce not found in 1M iterations");
}

const INTEX_READ_DATA_ABI = [
  {
    inputs: [{ name: "seriesId", type: "uint32" }],
    name: "readData",
    outputs: [
      {
        components: [
          { name: "promisLoadMinor", type: "uint128" },
          { name: "costAmountMinor", type: "uint64" },
          { name: "floorPriceMinor", type: "uint64" },
          { name: "issuedAt", type: "uint32" },
          { name: "calledAt", type: "uint32" },
          { name: "intexCallPeriod", type: "uint32" },
          { name: "totalSupply", type: "uint32" },
          { name: "issuedIntexCount", type: "uint32" },
          { name: "mintedCount", type: "uint32" },
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

export interface MineArgs {
  settlementAddress: Address;
  promisAddress: Address;
  intexAddress: Address;
  seriesId: number;
  amount: bigint;
}

export interface MineClient {
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

export async function minePromis(client: MineClient, args: MineArgs): Promise<Hex> {
  if (args.amount <= 0n) throw new Error("amount must be > 0");

  const settledId = await client.publicClient.readContract<bigint>({
    address: args.intexAddress,
    abi: INTEX_READ_ABI,
    functionName: "settledTokenId",
    args: [args.seriesId],
  });

  const settledBalance = await client.publicClient.readContract<bigint>({
    address: args.intexAddress,
    abi: INTEX_READ_ABI,
    functionName: "balanceOf",
    args: [client.walletClient.account.address, settledId],
  });

  console.log("[mine]", {
    seriesId: args.seriesId,
    holder: client.walletClient.account.address,
    settledBalance: settledBalance.toString(),
    requestedAmount: args.amount.toString(),
  });

  if (settledBalance < args.amount) {
    throw new Error(
      `Insufficient Settled balance: have ${settledBalance.toString()}, requested ${args.amount.toString()}`,
    );
  }

  const before = await client.publicClient.readContract<bigint>({
    address: args.promisAddress,
    abi: PROMIS_BALANCE_ABI,
    functionName: "balanceOf",
    args: [client.walletClient.account.address],
  });

  const seriesData = await client.publicClient.readContract<{ promisLoadMinor: bigint }>({
    address: args.intexAddress,
    abi: INTEX_READ_DATA_ABI,
    functionName: "readData",
    args: [args.seriesId],
  });
  const promisAmount = args.amount * seriesData.promisLoadMinor;
  const seq = await client.publicClient.readContract<number>({
    address: args.settlementAddress,
    abi: SETTLEMENT_ABI,
    functionName: "mineSeq",
    args: [args.seriesId, client.walletClient.account.address],
  });
  const seed = keccak256(
    encodePacked(
      ["address", "uint256", "uint32", "uint32"],
      [client.walletClient.account.address, promisAmount, args.seriesId, seq],
    ),
  );
  const nonce = findPoWNonce(seed);

  const tx = await client.walletClient.writeContract<Hex>({
    address: args.settlementAddress,
    abi: SETTLEMENT_ABI,
    functionName: "minePromis",
    args: [args.seriesId, args.amount, nonce],
    account: client.walletClient.account,
  });
  await client.publicClient.waitForTransactionReceipt({ hash: tx });

  const after = await client.publicClient.readContract<bigint>({
    address: args.promisAddress,
    abi: PROMIS_BALANCE_ABI,
    functionName: "balanceOf",
    args: [client.walletClient.account.address],
  });

  console.log("[mine] minePromis tx:", tx);
  console.log("[mine] Promis minted:", (after - before).toString());
  return tx;
}
