import assert from "node:assert/strict";
import { createServer } from "node:http";
import { once } from "node:events";
import test from "node:test";
import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import {
  type AbiParameter,
  type AbiFunction,
  type Chain,
  type Hex,
  type PublicClient,
  type WalletClient,
  decodeFunctionData,
  zeroHash,
} from "viem";
import { privateKeyToAccount } from "viem/accounts";
import {
  type Ctx,
  createCtx,
  formatNativeAmount,
  parseNativeAmount,
} from "./chain.js";
import { formatParam, humanizeReturn } from "./format.js";
import { humanizeOrder } from "./intent/format.js";
import { resolveContract } from "./registry.js";
import { registerSignTools } from "./tools/sign.js";

async function withChainIdRpc<T>(chainId: number, run: (rpcUrl: string) => Promise<T>): Promise<T> {
  const rpc = createServer(async (request, response) => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(Buffer.from(chunk));
    const payload = JSON.parse(Buffer.concat(chunks).toString("utf8")) as
      | { id: number }
      | { id: number }[];
    const reply = (item: { id: number }) => ({
      jsonrpc: "2.0",
      id: item.id,
      result: `0x${chainId.toString(16)}`,
    });
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify(Array.isArray(payload) ? payload.map(reply) : reply(payload)));
  });
  rpc.listen(0, "127.0.0.1");
  await once(rpc, "listening");

  try {
    const address = rpc.address();
    assert(address && typeof address === "object");
    return await run(`http://127.0.0.1:${address.port}`);
  } finally {
    rpc.close();
    await once(rpc, "close");
  }
}

test("Outbe chain metadata declares six native decimals", async () => {
  await withChainIdRpc(54_322_345, async (rpcUrl) => {
    const ctx = await createCtx(rpcUrl);
    assert.deepEqual(ctx.chain.nativeCurrency, { name: "COEN", symbol: "COEN", decimals: 6 });
  });
});

test("BSC chain metadata retains eighteen native decimals", async () => {
  await withChainIdRpc(97, async (rpcUrl) => {
    const ctx = await createCtx(rpcUrl);
    assert.deepEqual(ctx.chain.nativeCurrency, { name: "BNB", symbol: "BNB", decimals: 18 });
  });
});

test("network-native parsing and formatting follows chain metadata", () => {
  const outbe = { nativeCurrency: { decimals: 6 } } as Chain;
  const bsc = { nativeCurrency: { decimals: 18 } } as Chain;

  assert.equal(parseNativeAmount(outbe, "1.5"), 1_500_000n);
  assert.equal(formatNativeAmount(outbe, 1_500_000n), "1.5");
  assert.equal(parseNativeAmount(bsc, "1.5"), 1_500_000_000_000_000_000n);
  assert.equal(formatNativeAmount(bsc, 1_500_000_000_000_000_000n), "1.5");
});

test("MCP formats native monetary fields at scale6 and leaves dimensionless FP18 alone", () => {
  assert.deepEqual(
    formatParam({ name: "balance", type: "uint256" } as AbiParameter, 1_500_000n),
    { raw: "1500000", value: "1.5" },
  );
  assert.deepEqual(
    formatParam(
      { name: "rewardBand", type: "uint256" } as AbiParameter,
      20_000_000_000_000_000n,
    ),
    { raw: "20000000000000000", value: "0.02" },
  );
});

test("MCP formats Credis and Oracle annual rates with six decimals", () => {
  const position = {
    name: "position",
    type: "tuple",
    internalType: "struct ICredis.Position",
    components: [{ name: "currencyRate", type: "uint256" }],
  } as AbiParameter;
  assert.deepEqual(formatParam(position, { currencyRate: 43_000n }), {
    currencyRate: { raw: "43000", value: "0.043" },
  });

  const getCurrencyRate = {
    type: "function",
    name: "getCurrencyRate",
    stateMutability: "view",
    inputs: [{ name: "isoCode", type: "uint16" }],
    outputs: [{ name: "rate", type: "uint256" }],
  } as AbiFunction;
  assert.deepEqual(humanizeReturn(getCurrencyRate, 43_000n), {
    rate: { raw: "43000", value: "0.043" },
  });

  assert.deepEqual(
    formatParam({ name: "rate", type: "uint256" } as AbiParameter, 43_000_000_000_000_000n),
    { raw: "43000000000000000", value: "0.043" },
  );
});

test("MCP signed COEN inputs convert whole amounts to six-decimal units", async () => {
  type ToolHandler = (args: Record<string, unknown>) => Promise<unknown>;
  const handlers = new Map<string, ToolHandler>();
  const server = {
    tool(name: string, ...args: unknown[]) {
      handlers.set(name, args.at(-1) as ToolHandler);
    },
  } as unknown as McpServer;
  const account = privateKeyToAccount(
    "0x0000000000000000000000000000000000000000000000000000000000000001",
  );
  const sent: { data: Hex }[] = [];
  const ctx = {
    rpcUrl: "http://unused.invalid",
    chain: { id: 1 } as Chain,
    publicClient: {} as PublicClient,
    account,
    walletClient: {
      sendTransaction: async (transaction: { data: Hex }) => {
        sent.push(transaction);
        return `0x${"11".repeat(32)}` as Hex;
      },
    } as WalletClient,
  } satisfies Ctx;
  registerSignTools(server, ctx);

  const cases = [
    {
      tool: "staking_stake",
      contract: "staking",
      args: { validator: account.address, amount: "1.5", wait: false },
      amountIndex: 1,
    },
    {
      tool: "staking_unstake",
      contract: "staking",
      args: { amount: "1.5", wait: false },
      amountIndex: 0,
    },
    {
      tool: "agentreward_claim",
      contract: "agentreward",
      args: { amount: "1.5", wait: false },
      amountIndex: 0,
    },
  ] as const;

  for (const item of cases) {
    const callback = handlers.get(item.tool);
    assert(callback, `${item.tool} was registered`);
    await callback(item.args);
    const transaction = sent.at(-1);
    assert(transaction, `${item.tool} submitted a transaction`);
    const entry = resolveContract(item.contract);
    const decoded = decodeFunctionData({ abi: entry.abi, data: transaction.data });
    assert.equal(decoded.args?.[item.amountIndex], 1_500_000n, item.tool);
  }
});

test("external intent amounts retain their existing 18-decimal presentation", () => {
  const order = humanizeOrder({
    sender: zeroHash,
    recipient: zeroHash,
    inputToken: zeroHash,
    outputToken: zeroHash,
    amountIn: 1_000_000_000_000_000_000n,
    amountOut: 500_000_000_000_000_000n,
    senderNonce: 1n,
    originDomain: 1,
    destinationDomain: 2,
    destinationSettler: zeroHash,
    fillDeadline: 1,
    data: "0x",
  });

  assert.equal(order.amountIn.value1e18, "1");
  assert.equal(order.amountOut.value1e18, "0.5");
});
