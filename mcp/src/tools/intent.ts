import { epochIso } from "../format/time.js";
import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import {
  type Address,
  type Hex,
  encodeFunctionData,
  formatUnits,
  getAddress,
  pad,
  parseUnits,
} from "viem";
import { z } from "zod";
import { networkName, waitFlag } from "./schemas.js";
import { type Ctx, formatNativeAmount } from "../chain.js";
import { type Network, type NetworkResolver, networkResolver } from "../net/resolver.js";
import { receiptSummary, requireAccount, sendCall, waitForReceipt } from "../net/tx.js";
import { loadConfig } from "../config.js";
import { ensureAllowance, readDecimals } from "../net/erc20.js";
import { handler, ok } from "./util.js";
import {
  DEFAULT_FILL_DEADLINE_SECONDS,
  DEFAULT_ROUTER,
  ROUTER_ABI,
} from "../intent/registry.js";
import {
  ORDER_DATA_TYPE_HASH,
  type OrderData,
  bytes32ToAddress,
  computeOrderId,
  encodeOrderData,
  humanizeOrder,
  isNative,
  statusLabel,
} from "../intent/format.js";
import { derivePhase, refundPayload } from "../intent/order.js";
import { loadOrder, tokenBalance } from "../intent/reads.js";
import { resolveToken } from "../intent/tokens.js";

/** Intent order tools: open a cross-chain order, track its lifecycle, refund an expired one. */
interface IntentDeps {
  ctx: Ctx;
  router: Address;
  resolveNetwork: NetworkResolver;
}

const tokenArg = z.string().describe("token: symbol (USD, COEN, ...) or a 0x address");

export function registerIntentTools(server: McpServer, ctx: Ctx): void {
  const config = loadConfig();
  const deps: IntentDeps = {
    ctx,
    router: getAddress(config.intentRouter ?? DEFAULT_ROUTER),
    resolveNetwork: networkResolver(ctx, config),
  };
  registerOrderOpen(server, deps);
  registerOrderTrack(server, deps);
  registerOrderRefund(server, deps);
}

function registerOrderOpen(server: McpServer, { ctx, router, resolveNetwork }: IntentDeps): void {
  server.tool(
    "intent_order_open",
    "Open a cross-chain intent order on the intent Router (ERC-7683 `open`). Pulls/approves the input " +
      "ERC20 (or sends native value), deposits into The Compact, returns the deterministic orderId. " +
      "`amount_in`/`amount_out` are whole-token decimals; input decimals are read on origin and output " +
      "decimals on destination (override with `output_decimals`). Tokens are symbols or 0x addresses. " +
      "Requires OUTBE_PRIVATE_KEY.",
    {
      origin: networkName,
      destination: networkName,
      input_token: tokenArg,
      output_token: tokenArg,
      amount_in: z.string().describe('input amount in whole tokens, e.g. "10" or "1.5"'),
      amount_out: z.string().optional().describe("output amount (default = amount_in)"),
      output_decimals: z.number().int().optional().describe("override dest output-token decimals"),
      recipient: z.string().optional().describe("recipient on dest chain (default = sender)"),
      fill_deadline_seconds: z
        .number()
        .int()
        .optional()
        .describe(`seconds until fill deadline (default ${DEFAULT_FILL_DEADLINE_SECONDS})`),
      wait: waitFlag,
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
      const approveTx = native
        ? null
        : await ensureAllowance(ctx, originNet, { token: input.address, spender: router, amount: amountIn });

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
        approveTx,
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
        fillDeadline: epochIso(fillDeadline),
      };
      if (a.wait === false) return ok({ ...meta, status: "submitted" });

      return ok({ ...meta, ...receiptSummary(await waitForReceipt(originNet, hash)) });
    }),
  );
}

function registerOrderTrack(server: McpServer, { router, resolveNetwork }: IntentDeps): void {
  server.tool(
    "intent_order_track",
    "Where an order is in its cross-chain lifecycle, as a deterministic snapshot (no event scan). " +
      "Reads origin/destination status and derives a coarse `phase` (OPENED -> CLAIMED -> FILLED -> SETTLED, " +
      "plus REFUNDED/EXPIRED) with a `next` hint. Poll it (e.g. via /loop) to follow progress.",
    {
      order_id: z.string().describe("0x-prefixed bytes32 order id"),
      chain: networkName.describe("network where the order was opened (origin)"),
    },
    handler(async (a) => {
      const orderId = a.order_id as Hex;
      const hint = await resolveNetwork(a.chain);
      const { origin, order } = await loadOrder(router, resolveNetwork, orderId, hint);
      const destination = await resolveNetwork(String(order.destinationDomain)).catch(() => undefined);
      const [originRaw, destRaw] = await Promise.all([
        origin.client.readContract({ address: router, abi: ROUTER_ABI, functionName: "orderStatus", args: [orderId] }) as Promise<Hex>,
        destination?.client.readContract({
          address: router,
          abi: ROUTER_ABI,
          functionName: "destinationOrderStatus",
          args: [orderId],
        }) as Promise<Hex> | undefined,
      ]);
      const originStatus = statusLabel(originRaw) || "UNKNOWN";
      const destinationStatus = (destRaw && statusLabel(destRaw)) || "UNKNOWN";

      // The user's own balances (poll twice to see a before/after delta).
      const user = bytes32ToAddress(order.sender);
      const [inputOnOrigin, outputOnDest] = await Promise.all([
        tokenBalance(origin, bytes32ToAddress(order.inputToken), user),
        destination ? tokenBalance(destination, bytes32ToAddress(order.outputToken), user) : null,
      ]);

      const now = Date.now() / 1000;
      const { phase, next } = derivePhase(originStatus, destinationStatus, now > order.fillDeadline);

      return ok({
        orderId,
        phase,
        next,
        originNetwork: origin.name,
        destinationNetwork: destination?.name ?? `chainId:${order.destinationDomain}`,
        originStatus,
        destinationStatus,
        fillDeadline: {
          ...epochIso(order.fillDeadline),
          expired: now > order.fillDeadline,
        },
        userBalances: { inputOnOrigin, outputOnDest },
        order: humanizeOrder(order),
      });
    }),
  );
}

function registerOrderRefund(server: McpServer, { ctx, router, resolveNetwork }: IntentDeps): void {
  server.tool(
    "intent_order_refund",
    "Refund an expired, still-OPENED order, returning the input back to the sender. Calls `refund` on the " +
      "destination router; cross-chain refunds pay the bridge messaging fee (quoted automatically), " +
      "same-chain refunds are free. Reverts if the order is not OPENED or the deadline has not passed. " +
      "Requires OUTBE_PRIVATE_KEY.",
    {
      order_id: z.string().describe("0x-prefixed bytes32 order id"),
      chain: networkName.describe("network where the order was opened (origin)"),
      wait: z.boolean().optional(),
    },
    handler(async (a) => {
      requireAccount(ctx);
      const orderId = a.order_id as Hex;
      const hint = await resolveNetwork(a.chain);
      const { origin, order, originData } = await loadOrder(router, resolveNetwork, orderId, hint);

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
        const payload = refundPayload(orderId);
        const fee = (await destNet.client.readContract({
          address: router,
          abi: ROUTER_ABI,
          functionName: "quote",
          args: [order.originDomain, payload],
        })) as bigint;
        value = fee;
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
        messagingFee: { raw: value.toString(), value: formatNativeAmount(destNet.chain, value) },
        recipient: bytes32ToAddress(order.sender),
      };
      if (a.wait === false) return ok({ ...meta, status: "submitted" });

      return ok({ ...meta, ...receiptSummary(await waitForReceipt(destNet, hash)) });
    }),
  );
}
