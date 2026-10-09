import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { networkName } from "../schemas.js";
import { handler, ok } from "../util.js";
import { holding } from "../../intex/holdings.js";
import { addr, ownedWithBalances } from "../../intex/reads.js";
import { NFT_ABI } from "../../intex/registry.js";
import { accountArg, seriesArg } from "./args.js";
import type { IntexDeps } from "./deps.js";

/** Intex NFT holdings on any network. */
export function registerHoldingsTools(server: McpServer, deps: IntexDeps): void {
  const { whoever, target } = deps;
  server.tool(
    "intex_holdings_by_owner",
    "Intex NFT holdings for an address: owned token ids, balances, decoded status (Issued/Settled), and " +
      "for Issued ones the series lifecycle with its settlementDeadline and, on outbe, whether it has qualified. " +
      "Defaults to bsc-testnet (where won NFTs " +
      "land); pass network to read outbe. A holding away from outbe cannot be settled where it sits - bridge " +
      "it over with intex_bridge_send before the deadline shown here.",
    { account: accountArg, network: networkName.optional() },
    handler(async ({ account, network }) => {
      const n = await target(network);
      const who = whoever(account);
      const [tokenIds, balances] = await ownedWithBalances(n, who);
      const holdings = await Promise.all(tokenIds.map((tokenId, i) => holding(n, tokenId, balances[i])));
      return ok({ network: n.name, account: who, count: holdings.length, holdings });
    }),
  );

  server.tool(
    "intex_series_balance",
    "An address's Intex NFT balance for one series, split into issued and settled token ids. Reads the " +
      "chain you ask for; settlement only happens on outbe, so an issued balance found elsewhere has to be " +
      "bridged over before the series settlementDeadline (intex_series_info shows it).",
    { series: seriesArg, account: accountArg, network: networkName.optional() },
    handler(async ({ series, account, network }) => {
      const n = await target(network);
      const who = whoever(account);
      const [issued, settled] = (await n.client.readContract({
        address: addr(n, "nft"),
        abi: NFT_ABI,
        functionName: "tokenIds",
        args: [series],
      })) as [bigint, bigint];
      const [issuedBal, settledBal] = (await Promise.all([
        n.client.readContract({ address: addr(n, "nft"), abi: NFT_ABI, functionName: "balanceOf", args: [who, issued] }),
        n.client.readContract({ address: addr(n, "nft"), abi: NFT_ABI, functionName: "balanceOf", args: [who, settled] }),
      ])) as [bigint, bigint];
      return ok({
        network: n.name,
        series,
        account: who,
        issued: { tokenId: issued.toString(), balance: issuedBal.toString() },
        settled: { tokenId: settled.toString(), balance: settledBal.toString() },
      });
    }),
  );
}
