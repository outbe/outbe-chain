import {
  type Abi,
  type AbiFunction,
  type Chain,
  type Hex,
  type PublicClient,
  type WalletClient,
  createPublicClient,
  createWalletClient,
  defineChain,
  encodeFunctionData,
  formatUnits,
  http,
  parseUnits,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import { nativeCurrencyForChainId } from "./net/chains.js";
import { requireAccount } from "./net/tx.js";
import type { ContractEntry } from "./registry.js";

export interface Ctx {
  rpcUrl: string;
  chain: Chain;
  publicClient: PublicClient;
  walletClient?: WalletClient;
  account?: ReturnType<typeof privateKeyToAccount>;
}

export function parseNativeAmount(chain: Pick<Chain, "nativeCurrency">, value: string): bigint {
  return parseUnits(value, chain.nativeCurrency.decimals);
}

export function formatNativeAmount(
  chain: Pick<Chain, "nativeCurrency">,
  value: bigint,
): string {
  return formatUnits(value, chain.nativeCurrency.decimals);
}

/**
 * Build the chain context. We read the chain id from the node (`eth_chainId`), so
 * we make no fork assumptions. The node supports EIP-1559 (block carries
 * baseFeePerGas), so transactions are type-2. viem fills the fee fields from the
 * node. We only override `gas`, because estimateGas cannot simulate the
 * in-enclave tribute decrypt.
 */
export async function createCtx(rpcUrl: string, privateKey?: string): Promise<Ctx> {
  const transport = http(rpcUrl);
  const probe = createPublicClient({ transport });
  const id = await probe.getChainId();

  const chain = defineChain({
    id,
    name: `outbe-${id}`,
    nativeCurrency: nativeCurrencyForChainId(id),
    rpcUrls: { default: { http: [rpcUrl] } },
  });

  const publicClient = createPublicClient({ chain, transport });

  let walletClient: WalletClient | undefined;
  let account: ReturnType<typeof privateKeyToAccount> | undefined;
  if (privateKey) {
    const pk = (privateKey.startsWith("0x") ? privateKey : `0x${privateKey}`) as Hex;
    account = privateKeyToAccount(pk);
    walletClient = createWalletClient({ account, chain, transport });
  }

  return { rpcUrl, chain, publicClient, walletClient, account };
}

/** The AbiFunction item for a method, used for argument coercion + humanizing. */
export function abiFn(abi: Abi, method: string): AbiFunction {
  const fn = abi.find((a) => a.type === "function" && a.name === method) as
    | AbiFunction
    | undefined;
  if (!fn) throw new Error(`method "${method}" not found on this contract`);
  return fn;
}

/** Coerce loosely-typed MCP args (strings/numbers) into viem-friendly values. */
export function coerceArgs(fn: AbiFunction, args: unknown[]): unknown[] {
  const inputs = fn.inputs ?? [];
  if (args.length !== inputs.length) {
    throw new Error(
      `${fn.name} expects ${inputs.length} arg(s), got ${args.length}: [${inputs
        .map((i) => `${i.name}:${i.type}`)
        .join(", ")}]`,
    );
  }
  return inputs.map((p, i) => coerceOne(p.type, args[i]));
}

function coerceOne(type: string, value: unknown): unknown {
  if (type.endsWith("[]")) {
    const base = type.slice(0, -2);
    const arr = Array.isArray(value) ? value : JSON.parse(String(value));
    return arr.map((v: unknown) => coerceOne(base, v));
  }
  if (type.startsWith("uint") || type.startsWith("int")) {
    if (typeof value === "bigint") return value;
    return BigInt(value as string | number);
  }
  if (type === "bool") return value === true || value === "true";
  // address, bytes*, string, tuple: pass through as-is.
  return value;
}

export interface ViewResult {
  fn: AbiFunction;
  result: unknown;
}

/** Read a view method; returns the AbiFunction (for humanizing) + raw result. */
export async function readView(
  ctx: Ctx,
  entry: ContractEntry,
  method: string,
  rawArgs: unknown[],
): Promise<ViewResult> {
  const fn = abiFn(entry.abi, method);
  const args = coerceArgs(fn, rawArgs);
  const result = await ctx.publicClient.readContract({
    address: entry.address,
    abi: entry.abi,
    functionName: method,
    args,
  });
  return { fn, result };
}

/** A state-changing precompile call under an explicit gas limit. */
export interface PrecompileWrite {
  entry: ContractEntry;
  method: string;
  args: unknown[];
  gas: bigint;
  value?: bigint;
}

/** Sign + send a state-changing method with an explicit gas limit. */
export async function sendTx(ctx: Ctx, { entry, method, args: rawArgs, gas, value = 0n }: PrecompileWrite): Promise<Hex> {
  const account = requireAccount(ctx);
  if (!ctx.walletClient) throw new Error(`no signer for ${ctx.chain.name}`);
  const fn = abiFn(entry.abi, method);
  const args = coerceArgs(fn, rawArgs);
  const data = encodeFunctionData({ abi: entry.abi, functionName: method, args });
  return ctx.walletClient.sendTransaction({
    account,
    chain: ctx.chain,
    to: entry.address,
    data,
    gas,
    value,
  });
}
