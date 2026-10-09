import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { encodeFunctionData, formatUnits, maxUint256, parseUnits } from "viem";
import { z } from "zod";
import { TARGET_NETWORK } from "../../net/chains.js";
import { requireAccount } from "../../net/tx.js";
import { networkName, waitFlag } from "../schemas.js";
import { handler, ok } from "../util.js";
import { ERC20_ABI } from "../../intex/registry.js";
import { addr } from "../../intex/reads.js";
import { accountArg } from "./args.js";
import type { IntexDeps } from "./deps.js";

/** The payment token allowance for the escrow. */
export function registerFundingTools(server: McpServer, deps: IntexDeps): void {
  const { ctx, resolveNetwork, whoever, submit, paymentMeta } = deps;
  server.tool(
    "intex_payment_allowance",
    "Payment-token allowance granted to the EscrowAdapter and the account's balance, with token decimals/symbol.",
    { account: accountArg, network: networkName.optional() },
    handler(async ({ account, network }) => {
      const n = await resolveNetwork(network ?? TARGET_NETWORK);
      const who = whoever(account);
      const token = addr(n, "paymentToken");
      const escrow = addr(n, "escrow");
      const [allowance, balance, decimals, symbol] = (await Promise.all([
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "allowance", args: [who, escrow] }),
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "balanceOf", args: [who] }),
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "decimals" }),
        n.client.readContract({ address: token, abi: ERC20_ABI, functionName: "symbol" }),
      ])) as [bigint, bigint, number, string];
      const d = Number(decimals);
      return ok({
        network: n.name,
        account: who,
        token: { address: token, symbol, decimals: d },
        escrow,
        allowance: { raw: allowance.toString(), value: formatUnits(allowance, d) },
        balance: { raw: balance.toString(), value: formatUnits(balance, d) },
      });
    }),
  );

  server.tool(
    "intex_payment_approve",
    "Manually approve the EscrowAdapter to pull the payment token. Usually unnecessary - auction_bid_reveal " +
      "auto-approves what it needs. Pass amount in token units (e.g. \"100\") or max=true. Requires OUTBE_PRIVATE_KEY.",
    {
      amount: z.string().optional().describe('token amount to approve, e.g. "100"'),
      max: z.boolean().optional().describe("approve the maximum instead of a fixed amount"),
      network: networkName.optional(),
      wait: waitFlag,
    },
    handler(async ({ amount, max, network, wait }) => {
      const n = await resolveNetwork(network ?? TARGET_NETWORK);
      requireAccount(ctx);
      if (!max && amount === undefined) throw new Error('pass amount (e.g. "100") or max=true');
      const value = max ? maxUint256 : parseUnits(amount as string, (await paymentMeta(n)).decimals);
      const token = addr(n, "paymentToken");
      const escrow = addr(n, "escrow");
      const data = encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [escrow, value] });
      const receipt = await submit(n, token, data, 0n, wait);
      return ok({ network: n.name, token, escrow, approved: max ? "max" : (amount as string), ...receipt });
    }),
  );
}
