import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import {
  type Address,
  type Hex,
  decodeAbiParameters,
  encodeAbiParameters,
  encodeFunctionData,
  formatUnits,
  getAddress,
  pad,
  parseAbiParameters,
  parseUnits,
} from "viem";
import { z } from "zod";
import { type Ctx, formatNativeAmount } from "../chain.js";
import { NETWORKS } from "../net/chains.js";
import { type Network, networkResolver } from "../net/resolver.js";
import { receiptSummary, requireAccount, sendCall, waitForReceipt } from "../net/tx.js";
import { loadConfig } from "../config.js";
import { handler, ok } from "./util.js";
import {
  DEFAULT_FILL_DEADLINE_SECONDS,
  DEFAULT_ROUTER,
  ERC20_ABI,
  ROUTER_ABI,
} from "../intent/registry.js";
import {
  ORDER_DATA_TYPE_HASH,
  type OrderData,
  bytes32ToAddress,
  computeOrderId,
  decodeOrderData,
  encodeOrderData,
  humanizeOrder,
  isNative,
  statusLabel,
} from "../intent/format.js";
import { resolveToken } from "../intent/tokens.js";

/**
 * Intent / cross-chain order tools (ERC-7683 LayerZeroRouter). User surface:
 * open an order, track its lifecycle, refund an expired one. Domain logic lives
 * in `src/intent/` (registry/format/tokens). Networks come from the NETWORKS
 * table. A resolved network reuses the connected `ctx` when the chain id matches.
 * Otherwise it opens a fresh client via `createCtx`.
 *
 * Env (optional): OUTBE_INTENT_ROUTER (router address override).
 */

export function registerIntentTools(server: McpServer, ctx: Ctx): void {
  const config = loadConfig();
  const router = getAddress(config.intentRouter ?? DEFAULT_ROUTER);
  const pk = config.privateKey;

  const resolveNetwork = networkResolver(ctx, pk);

  async function readDecimals(n: Network, token: Address): Promise<number> {
    if (isNative(token)) return n.chain.nativeCurrency.decimals;
    try {
      const d = await n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "decimals" });
      return Number(d);
    } catch {
      return 18;
    }
  }

  /** Current balance of `account` for `token` on a network (native or ERC20). */
  async function balanceOf(n: Network, token: Address, account: Address) {
    if (isNative(token)) {
      const bal = await n.client.getBalance({ address: account });
      return { account, network: n.name, token, balance: { raw: bal.toString(), value: formatNativeAmount(n.chain, bal) } };
    }
    const [decimals, bal] = await Promise.all([
      readDecimals(n, token),
      n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "balanceOf", args: [account] }) as Promise<bigint>,
    ]);
    return { account, network: n.name, token, balance: { raw: bal.toString(), value: formatUnits(bal, decimals) } };
  }

  /** Read openOrders on a hint network, else probe outbe/bsc. Then decode the order. */
  async function loadOrder(
    orderId: Hex,
    hint: Network,
  ): Promise<{ origin: Network; order: OrderData; originData: Hex }> {
    const candidates: Network[] = [hint];
    for (const def of NETWORKS) {
      try {
        candidates.push(await resolveNetwork(def.name));
      } catch {
        /* network unreachable - probe what we have */
      }
    }
    const seen = new Set<number>();
    for (const n of candidates) {
      if (seen.has(n.chainId)) continue;
      seen.add(n.chainId);
      const raw = (await n.client.readContract({
        address: router,
        abi: ROUTER_ABI,
        functionName: "openOrders",
        args: [orderId],
      })) as Hex;
      if (raw && raw !== "0x") {
        const [, orderBytes] = decodeAbiParameters([{ type: "bytes32" }, { type: "bytes" }], raw) as [Hex, Hex];
        return { origin: n, order: decodeOrderData(orderBytes), originData: orderBytes };
      }
    }
    throw new Error(`order not found: ${orderId}`);
  }

  const networkArg = z.string().describe(`network name (one of: ${NETWORKS.map((d) => d.name).join(", ")})`);
  const tokenArg = z.string().describe("token: symbol (USD, COEN, ...) or a 0x address");

  // --- create order ----------------------------------------------------------
  server.tool(
    "intent_order_open",
    "Open a cross-chain intent order on the LayerZeroRouter (ERC-7683 `open`). Pulls/approves the input " +
      "ERC20 (or sends native value), deposits into The Compact, returns the deterministic orderId. " +
      "`amount_in`/`amount_out` are whole-token decimals; input decimals are read on origin and output " +
      "decimals on destination (override with `output_decimals`). Tokens are symbols or 0x addresses. " +
      "Requires OUTBE_PRIVATE_KEY.",
    {
      origin: networkArg,
      destination: networkArg,
      input_token: tokenArg,
      output_token: tokenArg,
      amount_in: z.string().describe('input amount in whole tokens, e.g. "10" or "1.5"'),
      amount_out: z.string().optional().describe("output amount (default = amount_in)"),
      output_decimals: z.number().int().optional().describe("override dest output-token decimals"),
      recipient: z.string().optional().describe("recipient on dest chain (default = sender)"),
      fill_deadline_seconds: z.number().int().optional().describe("seconds until fill deadline (default 86400)"),
      wait: z.boolean().optional().describe("wait for the receipt (default true)"),
    },
    handler(async (a) => {
      const account = requireAccount(ctx);
      const user = account.address;
      const originNet = await resolveNetwork(a.origin);
      const destNet = await resolveNetwork(a.destination);
      const input = resolveToken(a.input_token, originNet);
      const output = resolveToken(a.output_token, destNet);
      const recipient = a.recipient ? getAddress(a.recipient) : user;
      const fillDeadline =
        Math.floor(Date.now() / 1000) + (a.fill_deadline_seconds ?? DEFAULT_FILL_DEADLINE_SECONDS);

      const inputDecimals = await readDecimals(originNet, input.address);
      const outputDecimals = a.output_decimals ?? (await readDecimals(destNet, output.address));
      const amountIn = parseUnits(a.amount_in, inputDecimals);
      const amountOut = parseUnits(a.amount_out ?? a.amount_in, outputDecimals);
      const native = isNative(input.address);

      // Approve the router to pull the ERC20 input (skip for native).
      let approveTx: Hex | undefined;
      if (!native) {
        const allowance = (await originNet.client.readContract({
          address: input.address,
          abi: ERC20_ABI,
          functionName: "allowance",
          args: [user, router],
        })) as bigint;
        if (allowance < amountIn) {
          const data = encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [router, amountIn] });
          approveTx = await sendCall(ctx, originNet, { to: input.address, data, value: 0n });
          await waitForReceipt(originNet, approveTx);
        }
      }

      const orderData: OrderData = {
        sender: pad(user, { size: 32 }),
        recipient: pad(recipient, { size: 32 }),
        inputToken: pad(input.address, { size: 32 }),
        outputToken: pad(output.address, { size: 32 }),
        amountIn,
        amountOut,
        senderNonce: BigInt(Date.now()),
        originDomain: originNet.chainId,
        destinationDomain: destNet.chainId,
        destinationSettler: pad(router, { size: 32 }),
        fillDeadline,
        data: "0x",
      };
      const orderId = computeOrderId(orderData);

      const data = encodeFunctionData({
        abi: ROUTER_ABI,
        functionName: "open",
        args: [{ fillDeadline, orderDataType: ORDER_DATA_TYPE_HASH, orderData: encodeOrderData(orderData) }],
      });
      const value = native ? amountIn : 0n;
      const hash = await sendCall(ctx, originNet, { to: router, data, value });

      const meta = {
        orderId,
        txHash: hash,
        approveTx: approveTx ?? null,
        router,
        origin: { network: originNet.name, chainId: originNet.chainId },
        destination: { network: destNet.name, chainId: destNet.chainId },
        sender: user,
        recipient,
        senderNonce: orderData.senderNonce.toString(),
        inputToken: { symbol: input.symbol, address: input.address, decimals: inputDecimals },
        outputToken: { symbol: output.symbol, address: output.address, decimals: outputDecimals },
        amountIn: { raw: amountIn.toString(), value: formatUnits(amountIn, inputDecimals) },
        amountOut: { raw: amountOut.toString(), value: formatUnits(amountOut, outputDecimals) },
        fillDeadline: { epoch: fillDeadline, iso: new Date(fillDeadline * 1000).toISOString() },
      };
      if (a.wait === false) return ok({ ...meta, status: "submitted" });

      return ok({ ...meta, ...receiptSummary(await waitForReceipt(originNet, hash)) });
    }),
  );

  // --- track order (lifecycle snapshot) --------------------------------------
  server.tool(
    "intent_order_track",
    "Where an order is in its cross-chain lifecycle, as a deterministic snapshot (no event scan). " +
      "Reads origin/destination status and derives a coarse `phase` (OPENED -> CLAIMED -> FILLED -> SETTLED, " +
      "plus REFUNDED/EXPIRED) with a `next` hint. Poll it (e.g. via /loop) to follow progress.",
    {
      order_id: z.string().describe("0x-prefixed bytes32 order id"),
      chain: networkArg.describe("network where the order was opened (origin)"),
    },
    handler(async (a) => {
      const orderId = a.order_id as Hex;
      const hint = await resolveNetwork(a.chain);
      const { origin, order } = await loadOrder(orderId, hint);
      let destResolved: Network | undefined;
      try {
        destResolved = await resolveNetwork(String(order.destinationDomain));
      } catch {
        /* destination chain not in NETWORKS - fall back to origin for the read */
      }
      const destNet = destResolved ?? origin;

      const [originRaw, destRaw] = await Promise.all([
        origin.client.readContract({ address: router, abi: ROUTER_ABI, functionName: "orderStatus", args: [orderId] }) as Promise<Hex>,
        destNet.client.readContract({ address: router, abi: ROUTER_ABI, functionName: "destinationOrderStatus", args: [orderId] }) as Promise<Hex>,
      ]);
      const originStatus = statusLabel(originRaw) || "UNKNOWN";
      const destinationStatus = statusLabel(destRaw) || "UNKNOWN";

      // The user's own balances (poll twice to see a before/after delta).
      const user = bytes32ToAddress(order.sender);
      const [inputOnOrigin, outputOnDest] = await Promise.all([
        balanceOf(origin, bytes32ToAddress(order.inputToken), user),
        balanceOf(destNet, bytes32ToAddress(order.outputToken), user),
      ]);

      const now = Date.now() / 1000;
      let phase: string;
      let next: string;
      if (originStatus === "SETTLED") {
        phase = "SETTLED";
        next = "done - solver paid on origin";
      } else if (originStatus === "REFUNDED") {
        phase = "REFUNDED";
        next = "done - input returned to user";
      } else if (destinationStatus === "FILLED") {
        phase = "FILLED";
        next = "awaiting settle message to origin -> SETTLED";
      } else if (destinationStatus === "CLAIMED") {
        phase = "CLAIMED";
        next = "winner is filling on destination";
      } else if (originStatus === "OPENED" && now > order.fillDeadline) {
        phase = "EXPIRED";
        next = "refundable via intent_order_refund";
      } else if (originStatus === "OPENED") {
        phase = "OPENED";
        next = "auction running on destination - waiting for a solver to claim & fill";
      } else {
        phase = originStatus;
        next = "-";
      }

      return ok({
        orderId,
        phase,
        next,
        originNetwork: origin.name,
        destinationNetwork: destResolved?.name ?? `chainId:${order.destinationDomain}`,
        originStatus,
        destinationStatus,
        fillDeadline: {
          epoch: order.fillDeadline,
          iso: new Date(Number(order.fillDeadline) * 1000).toISOString(),
          expired: now > order.fillDeadline,
        },
        userBalances: { inputOnOrigin, outputOnDest },
        order: humanizeOrder(order),
      });
    }),
  );

  // --- refund an expired order ----------------------------------------------
  server.tool(
    "intent_order_refund",
    "Refund an expired, still-OPENED order, returning the input back to the sender. Calls `refund` on the " +
      "destination router; cross-chain refunds pay the LayerZero messaging fee (quoted automatically), " +
      "same-chain refunds are free. Reverts if the order is not OPENED or the deadline has not passed. " +
      "Requires OUTBE_PRIVATE_KEY.",
    {
      order_id: z.string().describe("0x-prefixed bytes32 order id"),
      chain: networkArg.describe("network where the order was opened (origin)"),
      wait: z.boolean().optional(),
    },
    handler(async (a) => {
      requireAccount(ctx);
      const orderId = a.order_id as Hex;
      const hint = await resolveNetwork(a.chain);
      const { origin, order, originData } = await loadOrder(orderId, hint);

      const originStatusRaw = (await origin.client.readContract({
        address: router,
        abi: ROUTER_ABI,
        functionName: "orderStatus",
        args: [orderId],
      })) as Hex;
      if (statusLabel(originStatusRaw) !== "OPENED") {
        throw new Error(`order is ${statusLabel(originStatusRaw) || "UNKNOWN"}, only OPENED orders can be refunded`);
      }
      if (Date.now() / 1000 < order.fillDeadline) {
        const mins = Math.ceil((order.fillDeadline - Date.now() / 1000) / 60);
        throw new Error(`fill deadline not passed yet (~${mins} min remaining)`);
      }

      let destNet: Network;
      try {
        destNet = await resolveNetwork(String(order.destinationDomain));
      } catch {
        throw new Error(`destination chainId ${order.destinationDomain} is not reachable (outbe/bsc only)`);
      }

      const sameChain = order.originDomain === order.destinationDomain;
      let value = 0n;
      if (!sameChain) {
        // payload mirrors RouterMessage refund encoding: (bool false, bytes32[] ids, bytes[] [])
        const payload = encodeAbiParameters(parseAbiParameters("bool, bytes32[], bytes[]"), [false, [orderId], []]);
        const fee = (await destNet.client.readContract({
          address: router,
          abi: ROUTER_ABI,
          functionName: "quote",
          args: [order.originDomain, payload, false],
        })) as { nativeFee: bigint; lzTokenFee: bigint };
        value = fee.nativeFee;
      }

      const data = encodeFunctionData({
        abi: ROUTER_ABI,
        functionName: "refund",
        args: [[{ fillDeadline: order.fillDeadline, orderDataType: ORDER_DATA_TYPE_HASH, orderData: originData }]],
      });
      const hash = await sendCall(ctx, destNet, { to: router, data, value });

      const meta = {
        orderId,
        txHash: hash,
        refundNetwork: destNet.name,
        sameChain,
        lzFee: { raw: value.toString(), value: formatNativeAmount(destNet.chain, value) },
        recipient: bytes32ToAddress(order.sender),
      };
      if (a.wait === false) return ok({ ...meta, status: "submitted" });

      return ok({ ...meta, ...receiptSummary(await waitForReceipt(destNet, hash)) });
    }),
  );
}
