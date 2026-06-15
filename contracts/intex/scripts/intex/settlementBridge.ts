// Settlement Bridge Script
// Phase 1 of the settlement lifecycle: system bridge (markCalled → bridge all holders BSC → Outbe).
// Phase 2 (approve stablecoins → settle) is in settle.ts.
//
// Phase 1 Steps:
//   1. (Pre-requisite) Auction flow completed, Intex issued on BSC
//   2. Create series on Outbe's IntexNFT1155 (fallback — read params from BSC, create + markCalled)
//   3. Fund TargetMessenger on BSC for system bridge LZ fees
//   4. markSeriesCalled via Telosis → MSG_MARK_CALLED → BSC
//   5. Wait for MSG_MARK_CALLED delivery on BSC
//   6. Wait for system bridge (SEND_MULTI) delivery on Outbe
//   7. Verify holders migrated to Outbe

import type { Address, Hex, PublicClient as ViemPublicClient, WalletClient as ViemWalletClient } from "viem";
import { createPublicClient, createWalletClient, formatEther, getContract, http, parseEther } from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { bsc, bscTestnet } from "viem/chains";
import {
  addressToBytes32,
  ENDPOINT_NONCE_ABI,
  LZ_INFRA,
  NETWORK_TO_EID,
  OUTBE_CHAINS,
} from "../shared/layerzero.js";
import { getNetworkName } from "../shared/taskUtils.js";

// =============================================================================
// Types
// =============================================================================

export interface WalletAccount {
  address: `0x${string}`;
}

export interface TelosisSettlementContract {
  read: {
    bridgeAdapter(): Promise<Address>;
  };
  write: {
    markSeriesCalled(
      args: [number, Hex],
      opts: { account: WalletAccount; value: bigint },
    ): Promise<`0x${string}`>;
  };
}

export interface SettlementRuntime {
  telosisAddress: Address;
  telosis: TelosisSettlementContract;
  intexOutbeAddress?: Address;
  viem: {
    getContractAt(name: string, address: Address): Promise<unknown>;
    getPublicClient(): Promise<ViemPublicClient>;
    getWalletClients(): Promise<readonly ViemWalletClient[]>;
  };
  publicClient: {
    waitForTransactionReceipt(args: { hash: `0x${string}` }): Promise<void>;
  };
  outbePublicClient: ViemPublicClient;
  wallet: { account: WalletAccount };
}

// =============================================================================
// Minimal ABIs for BSC contract reads
// =============================================================================

const INTEX_READ_ABI = [
  {
    inputs: [{ name: "tokenId", type: "uint256" }],
    name: "getSeriesHoldersWithBalances",
    outputs: [
      { name: "holders", type: "address[]" },
      { name: "balances", type: "uint256[]" },
    ],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [{ name: "tokenId", type: "uint256" }],
    name: "seriesHolderCount",
    outputs: [{ type: "uint256" }],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [{ name: "seriesId", type: "uint32" }],
    name: "issuedTokenId",
    outputs: [{ type: "uint256" }],
    stateMutability: "pure",
    type: "function",
  },
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

const INTEX_CREATE_SERIES_ABI = [
  {
    inputs: [
      { name: "seriesId", type: "uint32" },
      { name: "issuedIntexCount", type: "uint32" },
      { name: "promisLoadMinor", type: "uint128" },
      { name: "costAmountMinor", type: "uint64" },
      { name: "floorPriceMinor", type: "uint64" },
      { name: "intexCallPeriod", type: "uint32" },
      { name: "settlementTokenAlias", type: "uint16" },
      {
        name: "trigger",
        type: "tuple",
        components: [
          { name: "windowDays", type: "uint16" },
          { name: "thresholdDays", type: "uint16" },
          { name: "coenPriceCallTrigger", type: "uint64" },
        ],
      },
    ],
    name: "createSeries",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
] as const;

const INTEX_MARK_CALLED_ABI = [
  {
    inputs: [{ name: "seriesId", type: "uint32" }],
    name: "markCalled",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
] as const;

const ACCESS_CONTROL_READ_ABI = [
  {
    inputs: [{ name: "role", type: "bytes32" }, { name: "account", type: "address" }],
    name: "hasRole",
    outputs: [{ type: "bool" }],
    stateMutability: "view",
    type: "function",
  },
  {
    inputs: [],
    name: "RELAYER_ROLE",
    outputs: [{ type: "bytes32" }],
    stateMutability: "view",
    type: "function",
  },
] as const;

const ACCESS_CONTROL_WRITE_ABI = [
  {
    inputs: [{ name: "role", type: "bytes32" }, { name: "account", type: "address" }],
    name: "grantRole",
    outputs: [],
    stateMutability: "nonpayable",
    type: "function",
  },
] as const;

const OAPP_PEER_ABI = [
  { inputs: [{ name: "eid", type: "uint32" }], name: "peers", outputs: [{ type: "bytes32" }], stateMutability: "view", type: "function" },
  { inputs: [], name: "BNB_EID", outputs: [{ type: "uint32" }], stateMutability: "view", type: "function" },
  { inputs: [], name: "OUTBE_EID", outputs: [{ type: "uint32" }], stateMutability: "view", type: "function" },
] as const;

// =============================================================================
// Constants
// =============================================================================

const DEFAULT_MSG_VALUE = 100_000_000_000_000_000n; // 0.1 COEN
const FEE_BUFFER_BPS = 50n; // 0.5%
const EMPTY_EXTRA_OPTIONS = "0x" as Hex;
const INTEX_STATE_NAMES = ["Issued", "Qualified", "Called"];
// ISO 4217 numeric alias of the settlement token (840 = USD).
const DEFAULT_SETTLEMENT_TOKEN_ALIAS = 840;

const FUND_THRESHOLD = parseEther("0.01");
const FUND_AMOUNT = parseEther("0.05");

const BSC_NETWORK_CONFIG: Record<string, { chain: typeof bscTestnet | typeof bsc; rpcEnv: string; pkEnv: string; defaultRpc: string }> = {
  bscTestnet: { chain: bscTestnet, rpcEnv: "BSC_TESTNET_RPC_URL", pkEnv: "BSC_TESTNET_PRIVATE_KEY", defaultRpc: "https://bsc-testnet.publicnode.com" },
  bsc: { chain: bsc, rpcEnv: "BSC_MAINNET_RPC_URL", pkEnv: "BSC_MAINNET_PRIVATE_KEY", defaultRpc: "https://bsc-dataseed1.binance.org" },
};

// =============================================================================
// Argument Types
// =============================================================================

type BigIntInput = string | number | bigint | undefined;

export interface MarkSeriesCalledArgs {
  seriesId: number;
  extraOptions?: Hex;
  msgValue?: BigIntInput;
}

export interface FundBridgeAdapterArgs {
  bridgeAdapterAddress: Address;
  networkId?: string;
  amount?: bigint;
}

export interface WaitForBridgeArgs {
  originMessengerAddress: Address;
  bscNetworkId?: string;
  outbeNetworkId?: string;
  pollIntervalMs?: number;
  maxPolls?: number;
}

export interface WaitForSystemBridgeArgs {
  intexOutbeAddress: Address;
  seriesId: number;
  expectedHolderCount: number;
  pollIntervalMs?: number;
  maxPolls?: number;
}

export interface VerifyMigrationArgs {
  seriesId: number;
  intexBscAddress: Address;
  intexOutbeAddress: Address;
  bscNetworkId?: string;
}

export interface CreateSeriesOnOutbeArgs {
  seriesId: number;
  intexBscAddress: Address;
  intexOutbeAddress: Address;
  bscNetworkId?: string;
}

export interface CheckSeriesArgs {
  seriesId: number;
  intexBscAddress?: Address;
  intexOutbeAddress?: Address;
  bscNetworkId?: string;
}

// =============================================================================
// Utilities
// =============================================================================

/** Derive a human-readable settlement deadline from `calledAt + intexCallPeriod`. */
function derivedCallDeadline(calledAt: number, intexCallPeriod: number): string {
  if (calledAt === 0) return "not set";
  const seconds = Number(calledAt) + (Number.isFinite(intexCallPeriod) ? intexCallPeriod : 0);
  return new Date(seconds * 1000).toISOString();
}

function toOptionalBigInt(input: BigIntInput): bigint | undefined {
  if (input === undefined) return undefined;
  if (typeof input === "bigint") return input;
  if (typeof input === "number") return BigInt(input);
  const trimmed = String(input).trim();
  return trimmed === "" ? undefined : BigInt(trimmed);
}

function makeBscClient(networkId: string) {
  const cfg = BSC_NETWORK_CONFIG[networkId];
  if (!cfg) throw new Error(`Unknown BSC network: ${networkId}`);
  const rpc = process.env[cfg.rpcEnv] ?? cfg.defaultRpc;
  return createPublicClient({ chain: cfg.chain, transport: http(rpc) });
}

function makeBscWalletClient(networkId: string) {
  const cfg = BSC_NETWORK_CONFIG[networkId];
  if (!cfg) throw new Error(`Unknown BSC network: ${networkId}`);
  const rpc = process.env[cfg.rpcEnv] ?? cfg.defaultRpc;
  const pk = process.env[cfg.pkEnv];
  if (!pk) throw new Error(`${cfg.pkEnv} required for ${networkId}`);
  const account = privateKeyToAccount(pk as `0x${string}`);
  const transport = http(rpc);
  return {
    public: createPublicClient({ chain: cfg.chain, transport }),
    wallet: createWalletClient({ account, chain: cfg.chain, transport }),
    account,
  };
}

// =============================================================================
// Runtime Factory
// =============================================================================

export interface SettlementRuntimeOpts {
  telosisAddress: string;
  intexOutbeAddress?: string;
}

export async function createSettlementRuntime(
  hre: unknown,
  opts: SettlementRuntimeOpts,
): Promise<SettlementRuntime> {
  const networkName = getNetworkName(hre);

  let viem: {
    getContractAt: (name: string, address: Address) => Promise<unknown>;
    getPublicClient: () => Promise<ViemPublicClient>;
    getWalletClients: () => Promise<readonly ViemWalletClient[]>;
  };
  let publicClient: ViemPublicClient;
  let wallet: ViemWalletClient;

  if (networkName in OUTBE_CHAINS) {
    const chain = OUTBE_CHAINS[networkName as keyof typeof OUTBE_CHAINS];
    const rpc = process.env.OUTBE_RPC_URL ?? chain.rpcUrls.default.http[0];
    const pk = process.env.OUTBE_PRIVATE_KEY;
    if (!pk) throw new Error("OUTBE_PRIVATE_KEY required for Outbe networks");
    const account = privateKeyToAccount(pk as `0x${string}`);
    const transport = http(rpc);
    publicClient = createPublicClient({ chain, transport });
    wallet = createWalletClient({ account, chain, transport }) as ViemWalletClient;
    const artifacts = (hre as { artifacts: { readArtifact: (name: string) => Promise<{ abi: unknown[] }> } }).artifacts;
    viem = {
      getContractAt: async (name: string, address: Address) => {
        const { abi } = await artifacts.readArtifact(name);
        return getContract({ address, abi, client: { public: publicClient, wallet } });
      },
      getPublicClient: async () => publicClient,
      getWalletClients: async () => [wallet],
    };
  } else {
    const hreTyped = hre as { network: { connect(): Promise<{ viem: typeof viem }> } };
    const connected = await hreTyped.network.connect();
    viem = connected.viem;
    publicClient = await viem.getPublicClient();
    const [w] = await viem.getWalletClients();
    wallet = w!;
  }

  const telosisAddress = opts.telosisAddress as Address;
  const telosisRaw = await viem.getContractAt("Desis", telosisAddress);
  const telosis = telosisRaw as unknown as TelosisSettlementContract;

  if (!wallet.account) {
    throw new Error("No wallet account available. Check your network configuration.");
  }

  return {
    telosisAddress,
    telosis,
    intexOutbeAddress: opts.intexOutbeAddress as Address | undefined,
    viem,
    publicClient: {
      waitForTransactionReceipt: async (args: { hash: Hex }) => {
        await publicClient.waitForTransactionReceipt(args);
      },
    },
    outbePublicClient: publicClient,
    wallet: { account: wallet.account as WalletAccount },
  };
}

// =============================================================================
// Phase 1: Create Series on Outbe (read params from BSC, create + markCalled)
// NOTE: In the standard flow, Telosis creates the series on Outbe during
//       sendIssuanceInstructions. This function is a fallback for cases where
//       the series was not created during the auction flow.
// =============================================================================

export async function createSeriesOnOutbe(
  runtime: SettlementRuntime,
  args: CreateSeriesOnOutbeArgs,
): Promise<void> {
  const netId = args.bscNetworkId ?? "bscTestnet";
  const bscPublic = makeBscClient(netId);
  const seriesId = args.seriesId;

  // Check if series already exists on Outbe
  try {
    const outbeData = await runtime.outbePublicClient.readContract({
      address: args.intexOutbeAddress,
      abi: INTEX_READ_ABI,
      functionName: "readData",
      args: [seriesId],
    });
    const d = outbeData as { issuedAt: number; state: number };
    if (d.issuedAt > 0) {
      console.log(`[create-series-outbe] Series already exists on Outbe (state: ${INTEX_STATE_NAMES[d.state]}), skipping`);
      return;
    }
  } catch {
    // UnknownSeriesId revert — series doesn't exist yet, continue
  }

  // Read series params from BSC
  console.log("[create-series-outbe] Reading series params from BSC...");
  const bscData = await bscPublic.readContract({
    address: args.intexBscAddress,
    abi: INTEX_READ_ABI,
    functionName: "readData",
    args: [seriesId],
  });

  const d = bscData as {
    promisLoadMinor: bigint;
    costAmountMinor: bigint;
    floorPriceMinor: bigint;
    intexCallPeriod: number;
    issuedIntexCount: number;
    settlementTokenAlias: number;
    intexCallTrigger: { windowDays: number; thresholdDays: number; coenPriceCallTrigger: bigint };
  };

  console.log("[create-series-outbe] BSC series params:", {
    promisLoadMinor: d.promisLoadMinor.toString(),
    costAmountMinor: d.costAmountMinor.toString(),
    floorPriceMinor: d.floorPriceMinor.toString(),
    issuedIntexCount: d.issuedIntexCount,
    seriesId,
  });

  // Ensure deployer has RELAYER_ROLE on Outbe IntexNFT1155
  const bridgeRole = await runtime.outbePublicClient.readContract({
    address: args.intexOutbeAddress,
    abi: ACCESS_CONTROL_READ_ABI,
    functionName: "RELAYER_ROLE",
  });

  const walletClient = (await runtime.viem.getWalletClients())[0]!;
  const account = walletClient.account!;
  const deployerAddr = account.address;

  const hasBridgeRole = await runtime.outbePublicClient.readContract({
    address: args.intexOutbeAddress,
    abi: ACCESS_CONTROL_READ_ABI,
    functionName: "hasRole",
    args: [bridgeRole, deployerAddr],
  });

  const addr = args.intexOutbeAddress as `0x${string}`;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any -- raw writeContract with minimal ABI
  const write = walletClient.writeContract.bind(walletClient) as (args: any) => Promise<`0x${string}`>;

  if (!hasBridgeRole) {
    console.log("[create-series-outbe] Granting RELAYER_ROLE to deployer...");
    const grantTx = await write({
      address: addr, abi: ACCESS_CONTROL_WRITE_ABI, functionName: "grantRole",
      args: [bridgeRole, deployerAddr], account,
    });
    await runtime.publicClient.waitForTransactionReceipt({ hash: grantTx });
    console.log("[create-series-outbe] RELAYER_ROLE granted (tx:", grantTx, ")");
  }

  console.log("[create-series-outbe] Creating series on Outbe IntexNFT1155...");
  // Mirror the BSC series params (intexCallPeriod 0 falls back to the contract default of 21 days).
  const intexCallPeriod = Number.isFinite(d.intexCallPeriod) ? d.intexCallPeriod : 0;
  const settlementTokenAlias = Number.isFinite(d.settlementTokenAlias)
    ? d.settlementTokenAlias
    : DEFAULT_SETTLEMENT_TOKEN_ALIAS;
  // BSC SeriesData carries the canonical issuedIntexCount set at the auction-cleared moment;
  // mirror it onto Outbe so the mint cap matches across chains.
  if (!Number.isFinite(d.issuedIntexCount) || d.issuedIntexCount <= 0) {
    throw new Error(
      `[create-series-outbe] BSC series ${seriesId} has no issuedIntexCount; cannot mirror to Outbe`,
    );
  }
  const createTx = await write({
    address: addr,
    abi: INTEX_CREATE_SERIES_ABI,
    functionName: "createSeries",
    args: [
      seriesId,
      d.issuedIntexCount,
      d.promisLoadMinor,
      d.costAmountMinor,
      d.floorPriceMinor,
      intexCallPeriod,
      settlementTokenAlias,
      d.intexCallTrigger,
    ],
    account,
  });
  await runtime.publicClient.waitForTransactionReceipt({ hash: createTx });
  console.log("[create-series-outbe] Series created (tx:", createTx, ")");

  console.log("[create-series-outbe] Marking series as Called on Outbe...");
  const markTx = await write({
    address: addr,
    abi: INTEX_MARK_CALLED_ABI,
    functionName: "markCalled",
    args: [seriesId],
    account,
  });
  await runtime.publicClient.waitForTransactionReceipt({ hash: markTx });
  console.log("[create-series-outbe] Series marked Called (tx:", markTx, ")");
}

// =============================================================================
// Phase 1: Fund TargetMessenger on BSC
// =============================================================================

export async function fundBridgeAdapter(args: FundBridgeAdapterArgs): Promise<void> {
  const netId = args.networkId ?? "bscTestnet";
  const bsc = makeBscWalletClient(netId);

  const balance = await bsc.public.getBalance({ address: args.bridgeAdapterAddress });
  const symbol = BSC_NETWORK_CONFIG[netId]!.chain.nativeCurrency.symbol;
  console.log(`[fund-adapter] TargetMessenger (${args.bridgeAdapterAddress}) balance: ${formatEther(balance)} ${symbol}`);

  const threshold = args.amount ?? FUND_THRESHOLD;
  if (balance >= threshold) {
    console.log(`[fund-adapter] Balance sufficient (>= ${formatEther(threshold)}), skipping`);
    return;
  }

  const fundAmount = args.amount ?? FUND_AMOUNT;
  console.log(`[fund-adapter] Sending ${formatEther(fundAmount)} to TargetMessenger...`);
  const tx = await bsc.wallet.sendTransaction({
    to: args.bridgeAdapterAddress,
    value: fundAmount,
    account: bsc.account,
  });
  await bsc.public.waitForTransactionReceipt({ hash: tx });
  const newBalance = await bsc.public.getBalance({ address: args.bridgeAdapterAddress });
  console.log(`[fund-adapter] Funded. New balance: ${formatEther(newBalance)} (tx: ${tx})`);
}

// =============================================================================
// Phase 1: markSeriesCalled
// =============================================================================

export async function markSeriesCalled(
  runtime: SettlementRuntime,
  args: MarkSeriesCalledArgs,
): Promise<void> {
  const { telosis, publicClient, wallet } = runtime;

  // The call deadline is no longer a parameter; it is derived on-chain from the series
  // `intexCallPeriod` once `markCalled` is applied (deadline = calledAt + intexCallPeriod).
  // TODO: dynamically calculate gas based on BSC holders count (~400K + 35K * holdersCount)
  //       and encode as extraOptions instead of relying on static enforced options (3M gas)
  const extraOptions = args.extraOptions ?? EMPTY_EXTRA_OPTIONS;

  let msgValue = toOptionalBigInt(args.msgValue);
  if (msgValue == null || msgValue === 0n) {
    try {
      const bridgeAddr = await telosis.read.bridgeAdapter();
      const bridge = (await runtime.viem.getContractAt("OriginMessenger", bridgeAddr)) as {
        read: { quoteSendMarkCalled: (args: unknown[]) => Promise<unknown> };
      };
      const fee = await bridge.read.quoteSendMarkCalled([args.seriesId, extraOptions, false]);
      const nativeFee = extractNativeFee(fee);
      if (nativeFee != null && nativeFee > 0n) {
        msgValue = nativeFee + (nativeFee * FEE_BUFFER_BPS) / 10000n;
        console.log("[mark-called] quoted fee:", msgValue.toString(), "wei");
      }
    } catch (e) {
      console.warn("[mark-called] Failed to quote LZ fee:", (e as Error).message);
    }
    if (msgValue == null || msgValue === 0n) msgValue = DEFAULT_MSG_VALUE;
  }

  console.log("[mark-called]", {
    seriesId: args.seriesId,
    msgValue: msgValue.toString(),
  });

  const tx = await telosis.write.markSeriesCalled(
    [args.seriesId, extraOptions],
    { account: wallet.account, value: msgValue },
  );
  await publicClient.waitForTransactionReceipt({ hash: tx });
  console.log("[mark-called] done tx:", tx);
}

function extractNativeFee(fee: unknown): bigint | undefined {
  if (fee != null && typeof fee === "object" && "nativeFee" in fee && typeof (fee as { nativeFee: unknown }).nativeFee === "bigint") {
    return (fee as { nativeFee: bigint }).nativeFee;
  }
  if (Array.isArray(fee) && typeof fee[0] === "bigint") return fee[0];
  if (fee != null && typeof fee === "object" && 0 in fee && typeof (fee as { 0: unknown })[0] === "bigint") {
    return (fee as { 0: bigint })[0];
  }
  return undefined;
}

// =============================================================================
// Phase 1: Wait for MSG_MARK_CALLED delivery (Outbe → BSC)
// =============================================================================

export async function waitForMarkCalledDelivery(
  runtime: SettlementRuntime,
  args: WaitForBridgeArgs,
): Promise<{ outbound: bigint; delivered: bigint }> {
  const pollMs = args.pollIntervalMs ?? 5_000;
  const maxPolls = args.maxPolls ?? 60;
  const netId = args.bscNetworkId ?? "bscTestnet";
  const outbeNetId = args.outbeNetworkId ?? "outbeDevnet";
  const srcEid = NETWORK_TO_EID[outbeNetId];
  if (srcEid == null) throw new Error(`Unknown Outbe network: ${outbeNetId}`);

  const outbePublic = runtime.outbePublicClient;

  const bnbEid = await outbePublic.readContract({
    address: args.originMessengerAddress,
    abi: OAPP_PEER_ABI,
    functionName: "BNB_EID",
  });
  const peerBytes32 = await outbePublic.readContract({
    address: args.originMessengerAddress,
    abi: OAPP_PEER_ABI,
    functionName: "peers",
    args: [bnbEid],
  });
  const peerAddress = ("0x" + peerBytes32.slice(-40)) as Address;

  const outbound = await outbePublic.readContract({
    address: LZ_INFRA.endpoint,
    abi: ENDPOINT_NONCE_ABI,
    functionName: "outboundNonce",
    args: [args.originMessengerAddress, bnbEid, peerBytes32],
  });

  const bscPublic = makeBscClient(netId);
  const senderBytes32 = addressToBytes32(args.originMessengerAddress as `0x${string}`);

  console.log(`[lz-wait] Waiting for nonce ${outbound} (MSG_MARK_CALLED) delivery on BSC...`);

  for (let i = 0; i < maxPolls; i++) {
    const delivered = await bscPublic.readContract({
      address: LZ_INFRA.endpoint,
      abi: ENDPOINT_NONCE_ABI,
      functionName: "lazyInboundNonce",
      args: [peerAddress, srcEid, senderBytes32],
    });

    if (delivered >= outbound) {
      console.log(`[lz-wait] MSG_MARK_CALLED delivered! lazyInboundNonce=${delivered}`);
      return { outbound, delivered };
    }

    if (i === 0 || i % 6 === 0) {
      console.log(`[lz-wait]   lazyInboundNonce=${delivered}, waiting for ${outbound}...`);
    }
    await new Promise((r) => setTimeout(r, pollMs));
  }

  const timeoutMsg = `[lz-wait] Timeout waiting for MSG_MARK_CALLED delivery (outbound=${outbound})`;
  console.error(timeoutMsg);
  throw new Error(timeoutMsg);
}

// =============================================================================
// Phase 1: Wait for system bridge (SEND_MULTI) delivery on Outbe
// =============================================================================

export async function waitForSystemBridgeDelivery(
  runtime: SettlementRuntime,
  args: WaitForSystemBridgeArgs,
): Promise<{ holderCount: number }> {
  const pollMs = args.pollIntervalMs ?? 5_000;
  const maxPolls = args.maxPolls ?? 60;
  // Issued token id for a series is `uint256(seriesId)`.
  const tokenId = BigInt(args.seriesId);

  console.log(`[system-bridge-wait] Waiting for ${args.expectedHolderCount} holders on Outbe IntexNFT1155...`);

  for (let i = 0; i < maxPolls; i++) {
    const count = await runtime.outbePublicClient.readContract({
      address: args.intexOutbeAddress,
      abi: INTEX_READ_ABI,
      functionName: "seriesHolderCount",
      args: [tokenId],
    });

    const holderCount = Number(count);
    if (holderCount >= args.expectedHolderCount) {
      console.log(`[system-bridge-wait] Holders migrated! count=${holderCount}`);
      return { holderCount };
    }

    if (i === 0 || i % 6 === 0) {
      console.log(`[system-bridge-wait]   holders on Outbe: ${holderCount}, waiting for ${args.expectedHolderCount}...`);
    }
    await new Promise((r) => setTimeout(r, pollMs));
  }

  const finalCount = await runtime.outbePublicClient.readContract({
    address: args.intexOutbeAddress,
    abi: INTEX_READ_ABI,
    functionName: "seriesHolderCount",
    args: [tokenId],
  });
  const timeoutMsg = `[system-bridge-wait] Timeout! holders on Outbe: ${Number(finalCount)}, expected: ${args.expectedHolderCount}`;
  console.error(timeoutMsg);
  throw new Error(timeoutMsg);
}

// =============================================================================
// Phase 1: Verify migration
// =============================================================================

export async function verifyMigration(
  runtime: SettlementRuntime,
  args: VerifyMigrationArgs,
): Promise<{
  bscHolders: { address: string; balance: string }[];
  outbeHolders: { address: string; balance: string }[];
}> {
  const netId = args.bscNetworkId ?? "bscTestnet";
  const bscPublic = makeBscClient(netId);
  // Issued token id for a series is `uint256(seriesId)`.
  const tokenId = BigInt(args.seriesId);

  // BSC side — should have 0 holders after migration
  const [bscAddrs, bscBals] = await bscPublic.readContract({
    address: args.intexBscAddress,
    abi: INTEX_READ_ABI,
    functionName: "getSeriesHoldersWithBalances",
    args: [tokenId],
  });

  const bscHolders = bscAddrs.map((addr: string, i: number) => ({
    address: addr,
    balance: bscBals[i].toString(),
  }));

  // Outbe side — should have holders
  const [outbeAddrs, outbeBals] = await runtime.outbePublicClient.readContract({
    address: args.intexOutbeAddress,
    abi: INTEX_READ_ABI,
    functionName: "getSeriesHoldersWithBalances",
    args: [tokenId],
  });

  const outbeHolders = outbeAddrs.map((addr: string, i: number) => ({
    address: addr,
    balance: outbeBals[i].toString(),
  }));

  console.log("\n[verify] BSC holders:", bscHolders.length === 0 ? "none (all migrated ✓)" : bscHolders);
  console.log("[verify] Outbe holders:", outbeHolders.length > 0 ? outbeHolders : "none ✗");

  return { bscHolders, outbeHolders };
}

// =============================================================================
// Views: Check series status
// =============================================================================

export async function checkSeriesStatus(
  runtime: SettlementRuntime,
  args: CheckSeriesArgs,
): Promise<void> {
  if (args.intexOutbeAddress) {
    try {
      const data = await runtime.outbePublicClient.readContract({
        address: args.intexOutbeAddress,
        abi: INTEX_READ_ABI,
        functionName: "readData",
        args: [args.seriesId],
      });
      const d = data as {
        promisLoadMinor: bigint;
        costAmountMinor: bigint;
        floorPriceMinor: bigint;
        issuedAt: number;
        calledAt: number;
        intexCallPeriod: number;
        state: number;
      };
      console.log("[status] IntexNFT1155 (Outbe):", {
        state: INTEX_STATE_NAMES[d.state] ?? `Unknown(${d.state})`,
        costAmountMinor: d.costAmountMinor.toString(),
        callDeadline: derivedCallDeadline(d.calledAt, d.intexCallPeriod),
      });

      const [holders, balances] = await runtime.outbePublicClient.readContract({
        address: args.intexOutbeAddress,
        abi: INTEX_READ_ABI,
        functionName: "getSeriesHoldersWithBalances",
        args: [BigInt(args.seriesId)],
      });
      console.log(`[status] Outbe holders: ${holders.length}`);
      for (let i = 0; i < holders.length; i++) {
        console.log(`   ${holders[i]}: ${balances[i]}`);
      }
    } catch {
      console.log("[status] IntexNFT1155 (Outbe): token not yet created (will be bridged from BSC)");
    }
  }

  if (args.intexBscAddress) {
    try {
      const netId = args.bscNetworkId ?? "bscTestnet";
      const bscPublic = makeBscClient(netId);

      const data = await bscPublic.readContract({
        address: args.intexBscAddress,
        abi: INTEX_READ_ABI,
        functionName: "readData",
        args: [args.seriesId],
      });
      const d = data as { state: number; calledAt: number; intexCallPeriod: number };
      console.log("[status] IntexNFT1155 (BSC):", {
        state: INTEX_STATE_NAMES[d.state] ?? `Unknown(${d.state})`,
        callDeadline: derivedCallDeadline(d.calledAt, d.intexCallPeriod),
      });

      const [holders, balances] = await bscPublic.readContract({
        address: args.intexBscAddress,
        abi: INTEX_READ_ABI,
        functionName: "getSeriesHoldersWithBalances",
        args: [BigInt(args.seriesId)],
      });
      console.log(`[status] BSC holders: ${holders.length}`);
      for (let i = 0; i < holders.length; i++) {
        console.log(`   ${holders[i]}: ${balances[i]}`);
      }
    } catch {
      console.log("[status] IntexNFT1155 (BSC): token not found");
    }
  }
}

// =============================================================================
// Helper: read BSC holder count (used in interactive flow)
// =============================================================================

export async function getOutbeSeriesState(
  runtime: SettlementRuntime,
  seriesId: number,
): Promise<number | undefined> {
  if (!runtime.intexOutbeAddress) return undefined;
  try {
    const data = await runtime.outbePublicClient.readContract({
      address: runtime.intexOutbeAddress,
      abi: INTEX_READ_ABI,
      functionName: "readData",
      args: [seriesId],
    });
    return (data as { state: number }).state;
  } catch {
    return undefined;
  }
}

export async function getBscHolderCount(
  intexBscAddress: Address,
  seriesId: number,
  bscNetworkId: string,
): Promise<number> {
  const bscPublic = makeBscClient(bscNetworkId);
  // Issued token id for a series is `uint256(seriesId)`.
  const count = await bscPublic.readContract({
    address: intexBscAddress,
    abi: INTEX_READ_ABI,
    functionName: "seriesHolderCount",
    args: [BigInt(seriesId)],
  });
  return Number(count);
}
