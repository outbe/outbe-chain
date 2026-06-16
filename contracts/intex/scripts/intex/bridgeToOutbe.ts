// Bridge Issued Intex from BSC to Outbe via the user-driven ONFT1155Adapter.
//
// Pre-conditions:
//   - The series is in `Qualified` state on the source chain (bridge gate).
//   - The series exists on the destination chain (mirrored by Telosis at issuance).
//   - The caller is the holder of the Intex being moved.
//
// The adapter quotes the LayerZero fee, the caller pays it as `msg.value`, and the
// destination chain crosschain-mints the same `tokenId` to the same holder once the LZ
// packet is delivered.

import type { Address, Hex } from "viem";

const ONFT_ADAPTER_ABI = [
  {
    inputs: [
      {
        components: [
          { name: "dstEid", type: "uint32" },
          { name: "to", type: "bytes32" },
          { name: "tokenId", type: "uint256" },
          { name: "amount", type: "uint256" },
          { name: "extraOptions", type: "bytes" },
          { name: "composeMsg", type: "bytes" },
        ],
        name: "_sendParam",
        type: "tuple",
      },
      { name: "_payInLzToken", type: "bool" },
    ],
    name: "quoteSend",
    outputs: [
      {
        components: [
          { name: "nativeFee", type: "uint256" },
          { name: "lzTokenFee", type: "uint256" },
        ],
        type: "tuple",
      },
    ],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [
      {
        components: [
          { name: "dstEid", type: "uint32" },
          { name: "to", type: "bytes32" },
          { name: "tokenId", type: "uint256" },
          { name: "amount", type: "uint256" },
          { name: "extraOptions", type: "bytes" },
          { name: "composeMsg", type: "bytes" },
        ],
        name: "_sendParam",
        type: "tuple",
      },
      {
        components: [
          { name: "nativeFee", type: "uint256" },
          { name: "lzTokenFee", type: "uint256" },
        ],
        name: "_fee",
        type: "tuple",
      },
      { name: "_refundAddress", type: "address" },
    ],
    name: "send",
    outputs: [
      {
        components: [
          { name: "guid", type: "bytes32" },
          { name: "nonce", type: "uint64" },
          {
            components: [
              { name: "nativeFee", type: "uint256" },
              { name: "lzTokenFee", type: "uint256" },
            ],
            name: "fee",
            type: "tuple",
          },
        ],
        type: "tuple",
      },
    ],
    stateMutability: "payable",
    type: "function",
  },
] as const;

export interface BridgeToOutbeArgs {
  /** ONFT1155Adapter address on the source chain (e.g. BSC). */
  adapterAddress: Address;
  /** Destination LayerZero EID (e.g. Outbe). */
  dstEid: number;
  /** Series id whose Issued Intex token (tokenId = uint256(seriesId)) is bridged. */
  seriesId: number;
  /** Amount of Intex to send. */
  amount: bigint;
  /** Optional override for the destination recipient (default: caller). */
  recipient?: Address;
  /** LayerZero `extraOptions` (defaults to empty — relies on enforced options). */
  extraOptions?: Hex;
  /** Multiplicative buffer over the quoted native fee (basis points, default 50). */
  feeBufferBps?: bigint;
}

export interface BridgeClient {
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
      value?: bigint;
    }): Promise<T>;
    account: { address: Address };
  };
}

const EMPTY_EXTRA_OPTIONS = "0x" as Hex;
const DEFAULT_FEE_BUFFER_BPS = 50n; // 0.5%

function addressToBytes32(addr: Address): Hex {
  return ("0x" + addr.slice(2).toLowerCase().padStart(64, "0")) as Hex;
}

export async function bridgeIntexToOutbe(client: BridgeClient, args: BridgeToOutbeArgs): Promise<Hex> {
  const recipient = args.recipient ?? client.walletClient.account.address;
  const extraOptions = args.extraOptions ?? EMPTY_EXTRA_OPTIONS;
  const tokenId = BigInt(args.seriesId);

  const sendParam = {
    dstEid: args.dstEid,
    to: addressToBytes32(recipient),
    tokenId,
    amount: args.amount,
    extraOptions,
    composeMsg: "0x" as Hex,
  } as const;

  const quoted = await client.publicClient.readContract<{ nativeFee: bigint; lzTokenFee: bigint }>({
    address: args.adapterAddress,
    abi: ONFT_ADAPTER_ABI,
    functionName: "quoteSend",
    args: [sendParam, false],
  });
  const buffer = args.feeBufferBps ?? DEFAULT_FEE_BUFFER_BPS;
  const value = quoted.nativeFee + (quoted.nativeFee * buffer) / 10000n;

  console.log("[bridge-to-outbe]", {
    adapter: args.adapterAddress,
    dstEid: args.dstEid,
    tokenId: tokenId.toString(),
    amount: args.amount.toString(),
    recipient,
    nativeFee: quoted.nativeFee.toString(),
    valueWithBuffer: value.toString(),
  });

  const tx = await client.walletClient.writeContract<Hex>({
    address: args.adapterAddress,
    abi: ONFT_ADAPTER_ABI,
    functionName: "send",
    args: [sendParam, quoted, recipient],
    account: client.walletClient.account,
    value,
  });
  await client.publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[bridge-to-outbe] send tx:", tx);
  return tx;
}
