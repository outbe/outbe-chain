import {
  type Abi,
  type AbiFunction,
  type AbiParameter,
  type Address,
  type Hex,
  decodeFunctionData,
  encodeFunctionResult,
  getAddress,
  keccak256,
  numberToHex,
  parseTransaction,
  toFunctionSelector,
} from "viem";

/** A transaction the fake chain accepted, decoded back to its call. */
export interface SentTransaction {
  chainId: number;
  to: Address;
  function: string;
  args: unknown;
  value: string;
  gas: string;
  hash: Hex;
}

type Result = unknown | ((args: readonly unknown[]) => unknown);

interface Registered {
  address?: Address;
  abi: Abi;
}

const BLOCK_NUMBER = 100;
const BLOCK_TIMESTAMP = 1_760_000_000;
const GAS_PRICE = "0x3b9aca00";
const ZERO_BLOOM = `0x${"00".repeat(256)}`;
const word = (byte: string): Hex => `0x${byte.repeat(32)}`;

/** A deterministic value of any ABI type. */
export function sample(param: AbiParameter): unknown {
  const array = /^(.*)\[(\d*)\]$/.exec(param.type);
  if (array) {
    const inner = { ...param, type: array[1] } as AbiParameter;
    return Array.from({ length: array[2] ? Number(array[2]) : 2 }, () => sample(inner));
  }
  if (param.type === "tuple") {
    const components = (param as { components: readonly AbiParameter[] }).components;
    return components.map(sample);
  }
  const integer = /^u?int(\d*)$/.exec(param.type);
  if (integer) {
    const bits = Number(integer[1] || 256);
    if (bits === 8) return 2n;
    if (bits === 16) return 840n;
    if (bits === 32) return 20261009n;
    if (bits === 64) return BigInt(BLOCK_TIMESTAMP);
    return 3n;
  }
  const bytes = /^bytes(\d+)$/.exec(param.type);
  if (bytes) return `0x${"ab".repeat(Number(bytes[1]))}`;
  if (param.type === "address") return getAddress(`0x${"11".repeat(20)}`);
  if (param.type === "bool") return true;
  if (param.type === "bytes") return "0x1234";
  if (param.type === "string") return "text";
  throw new Error(`no sample for ${param.type}`);
}

function sampleResult(fn: AbiFunction): unknown {
  const outputs = fn.outputs ?? [];
  if (outputs.length === 1) return sample(outputs[0]);
  return outputs.map(sample);
}

class RpcError extends Error {
  constructor(
    readonly code: number,
    message: string,
  ) {
    super(message);
  }
}

/** A JSON-RPC chain behind a stubbed `fetch`, so even hardcoded RPC URLs land here. */
export class FakeChain {
  readonly sent: SentTransaction[] = [];
  readonly logs: unknown[] = [];
  private readonly registered: Registered[] = [];
  private readonly results = new Map<string, Result>();
  private readonly reverting = new Set<string>();
  private readonly receipts = new Map<Hex, Record<string, unknown>>();
  private readonly transactions = new Map<Hex, Record<string, unknown>>();
  private readonly nonces = new Map<number, number>();

  constructor(
    private readonly chainIds: Record<string, number>,
    private readonly sender: Address,
  ) {}

  register(abi: Abi, address?: string): void {
    this.registered.push({ abi, address: address ? getAddress(address) : undefined });
  }

  /** Overrides what `fn` returns, at `address` or anywhere. */
  reply(fn: string, result: Result, address?: string): void {
    this.results.set(this.key(fn, address), result);
  }

  revert(fn: string, address?: string): void {
    this.reverting.add(this.key(fn, address));
  }

  install(): () => void {
    const original = globalThis.fetch;
    globalThis.fetch = (async (input: string | URL | Request, init?: RequestInit) => {
      const url = String(input instanceof Request ? input.url : input).replace(/\/$/, "");
      const chainId = this.chainIds[url];
      if (chainId === undefined) throw new Error(`unexpected RPC ${url}`);
      const body = JSON.parse(String(init?.body)) as RpcRequest | RpcRequest[];
      const answer = (request: RpcRequest) => this.answer(chainId, request);
      const reply = Array.isArray(body) ? body.map(answer) : answer(body);
      return new Response(JSON.stringify(reply), { headers: { "content-type": "application/json" } });
    }) as typeof fetch;
    return () => {
      globalThis.fetch = original;
    };
  }

  /** Transactions sent since `from`, in order. */
  since(from: number): SentTransaction[] {
    return this.sent.slice(from);
  }

  private key(fn: string, address?: string): string {
    return address ? `${getAddress(address)}:${fn}` : fn;
  }

  private answer(chainId: number, request: RpcRequest) {
    try {
      return { jsonrpc: "2.0", id: request.id, result: this.handle(chainId, request.method, request.params ?? []) };
    } catch (error) {
      const code = error instanceof RpcError ? error.code : -32603;
      return { jsonrpc: "2.0", id: request.id, error: { code, message: (error as Error).message, data: "0x" } };
    }
  }

  private handle(chainId: number, method: string, params: unknown[]): unknown {
    switch (method) {
      case "eth_chainId":
        return numberToHex(chainId);
      case "eth_blockNumber":
        return numberToHex(BLOCK_NUMBER);
      case "eth_getBlockByNumber":
      case "eth_getBlockByHash":
        return block();
      case "eth_gasPrice":
      case "eth_maxPriorityFeePerGas":
        return GAS_PRICE;
      case "eth_estimateGas":
        return numberToHex(100_000);
      case "eth_getBalance":
        return numberToHex(10n ** 18n);
      case "eth_getCode":
        return "0x6000";
      case "eth_getTransactionCount":
        return numberToHex(this.nonces.get(chainId) ?? 0);
      case "eth_getLogs":
        return this.logs;
      case "eth_call":
        return this.call(params[0] as { to: Address; data: Hex });
      case "eth_sendRawTransaction":
        return this.send(chainId, params[0] as Hex);
      case "eth_getTransactionReceipt":
        return this.receipts.get(params[0] as Hex) ?? null;
      case "eth_getTransactionByHash":
        return this.transactions.get(params[0] as Hex) ?? null;
      default:
        throw new RpcError(-32601, `method ${method} is not faked`);
    }
  }

  private find(to: Address, data: Hex): AbiFunction {
    const selector = data.slice(0, 10);
    const target = getAddress(to);
    const ordered = [
      ...this.registered.filter((r) => r.address === target),
      ...this.registered.filter((r) => r.address === undefined),
    ];
    for (const { abi } of ordered) {
      const fn = abi.find(
        (item): item is AbiFunction => item.type === "function" && toFunctionSelector(item) === selector,
      );
      if (fn) return fn;
    }
    throw new RpcError(3, `execution reverted: unknown selector ${selector} at ${target}`);
  }

  private call({ to, data }: { to: Address; data: Hex }): Hex {
    const fn = this.find(to, data);
    if (this.reverting.has(this.key(fn.name, to)) || this.reverting.has(fn.name)) {
      throw new RpcError(3, `execution reverted: ${fn.name}`);
    }
    const { args } = decodeFunctionData({ abi: [fn], data });
    const override = this.results.get(this.key(fn.name, to)) ?? this.results.get(fn.name);
    const result =
      override === undefined
        ? sampleResult(fn)
        : typeof override === "function"
          ? (override as (args: readonly unknown[]) => unknown)(args ?? [])
          : override;
    return encodeFunctionResult({ abi: [fn], functionName: fn.name, result } as never);
  }

  private send(chainId: number, raw: Hex): Hex {
    const tx = parseTransaction(raw);
    const hash = keccak256(raw);
    const to = getAddress(tx.to as Address);
    let fn = "transfer";
    let args: unknown = [];
    if (tx.data && tx.data !== "0x") {
      const abiFn = this.find(to, tx.data);
      fn = abiFn.name;
      args = decodeFunctionData({ abi: [abiFn], data: tx.data }).args ?? [];
    }
    this.sent.push({
      chainId,
      to,
      function: fn,
      args,
      value: (tx.value ?? 0n).toString(),
      gas: (tx.gas ?? 0n).toString(),
      hash,
    });
    this.nonces.set(chainId, (this.nonces.get(chainId) ?? 0) + 1);
    this.record(hash, chainId, to, tx.data ?? "0x", tx.value ?? 0n, tx.gas ?? 0n);
    return hash;
  }

  /** Makes `hash` a mined transaction to `to` with `data`, as if it had been sent earlier. */
  seed(hash: Hex, chainId: number, to: string, data: Hex): void {
    this.record(hash, chainId, getAddress(to), data, 0n, 21_000n);
  }

  private record(hash: Hex, chainId: number, to: Address, data: Hex, value: bigint, gas: bigint): void {
    this.receipts.set(hash, {
      transactionHash: hash,
      transactionIndex: "0x0",
      blockHash: word("bb"),
      blockNumber: numberToHex(BLOCK_NUMBER),
      from: this.sender,
      to,
      cumulativeGasUsed: numberToHex(21_000),
      gasUsed: numberToHex(21_000),
      effectiveGasPrice: GAS_PRICE,
      contractAddress: null,
      logs: [],
      logsBloom: ZERO_BLOOM,
      status: "0x1",
      type: "0x2",
    });
    this.transactions.set(hash, {
      hash,
      blockHash: word("bb"),
      blockNumber: numberToHex(BLOCK_NUMBER),
      transactionIndex: "0x0",
      from: this.sender,
      to,
      value: numberToHex(value),
      gas: numberToHex(gas),
      input: data,
      nonce: "0x0",
      type: "0x2",
      chainId: numberToHex(chainId),
      maxFeePerGas: GAS_PRICE,
      maxPriorityFeePerGas: GAS_PRICE,
      r: word("01"),
      s: word("02"),
      v: "0x0",
      yParity: "0x0",
      accessList: [],
    });
  }
}

interface RpcRequest {
  id: number;
  method: string;
  params?: unknown[];
}

function block() {
  return {
    number: numberToHex(BLOCK_NUMBER),
    hash: word("aa"),
    parentHash: word("a9"),
    timestamp: numberToHex(BLOCK_TIMESTAMP),
    baseFeePerGas: GAS_PRICE,
    gasLimit: numberToHex(32_000_000),
    gasUsed: "0x0",
    miner: `0x${"00".repeat(20)}`,
    difficulty: "0x0",
    totalDifficulty: "0x0",
    extraData: "0x",
    logsBloom: ZERO_BLOOM,
    mixHash: word("00"),
    nonce: "0x0000000000000000",
    receiptsRoot: word("00"),
    sha3Uncles: word("00"),
    stateRoot: word("00"),
    transactionsRoot: word("00"),
    size: "0x0",
    transactions: [],
    uncles: [],
  };
}
