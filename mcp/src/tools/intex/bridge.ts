import type { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { encodeFunctionData, getAddress } from "viem";
import { formatNativeAmount } from "../../chain.js";
import { requireAccount } from "../../net/tx.js";
import { networkName, waitFlag } from "../schemas.js";
import { handler, ok } from "../util.js";
import { NFT_BRIDGE_ABI } from "../../intex/registry.js";
import { addr, bridgeSendParam } from "../../intex/reads.js";
import { destinationArg, recipientArg, seriesArg, unitsArg } from "./args.js";
import type { IntexDeps } from "./deps.js";

/** Bridging an Intex NFT between Intex chains. */
export function registerBridgeTools(server: McpServer, deps: IntexDeps): void {
  const { ctx, whoever, submit, target, bridgeDestination } = deps;
  server.tool(
    "intex_bridge_quote",
    "Bridge native fee to move an Intex NFT to another Intex chain, by default to outbe. Bridging is " +
      "owner-initiated at every stage: " +
      "to any recipient while the series is Issued or Qualified, and to yourself only once it is Called, up to " +
      "its settlementDeadline (read it with intex_series_info).",
    { series: seriesArg, units: unitsArg, recipient: recipientArg, network: networkName.optional(), destination: destinationArg },
    handler(async ({ series, units, recipient, network, destination }) => {
      const n = await target(network);
      const to = recipient ? getAddress(recipient) : whoever();
      const dstChainId = await bridgeDestination(n, destination);
      const sp = await bridgeSendParam(n, { series, units: BigInt(units), recipient: to, dstChainId });
      const fee = await n.client.readContract({
        address: addr(n, "nftBridge"),
        abi: NFT_BRIDGE_ABI,
        functionName: "quoteSend",
        args: [sp],
      });
      return ok({
        network: n.name,
        series,
        tokenId: sp.tokenId.toString(),
        dstChainId: sp.dstChainId,
        recipient: to,
        fee: { nativeFee: { raw: fee.toString(), value: formatNativeAmount(n.chain, fee) } },
      });
    }),
  );

  server.tool(
    "intex_bridge_send",
    "Bridge an Intex NFT to another Intex chain, by default to outbe, where settlement happens - nothing moves it for you, so a " +
      "position left on BSC past the series settlementDeadline can no longer be settled at all. Works at every " +
      "stage: to any recipient while Issued or Qualified, and to yourself only once the series is Called " +
      "(ownership is frozen then, so a recipient other than you is refused). The bridge burns your token " +
      "directly (role-gated), so no approval is needed. Auto-quotes the native fee (paid as value), which " +
      "you pay in the source chain's native token. Requires OUTBE_PRIVATE_KEY.",
    {
      series: seriesArg,
      units: unitsArg,
      recipient: recipientArg,
      network: networkName.optional(),
      destination: destinationArg,
      wait: waitFlag,
    },
    handler(async ({ series, units, recipient, network, destination, wait }) => {
      const n = await target(network);
      const account = requireAccount(ctx);
      const bridge = addr(n, "nftBridge");
      const to = recipient ? getAddress(recipient) : account.address;
      const dstChainId = await bridgeDestination(n, destination);
      const sp = await bridgeSendParam(n, { series, units: BigInt(units), recipient: to, dstChainId });
      const fee = await n.client.readContract({
        address: bridge,
        abi: NFT_BRIDGE_ABI,
        functionName: "quoteSend",
        args: [sp],
      });
      const data = encodeFunctionData({ abi: NFT_BRIDGE_ABI, functionName: "send", args: [sp] });
      const receipt = await submit(n, bridge, data, fee, wait);
      return ok({
        network: n.name,
        series,
        tokenId: sp.tokenId.toString(),
        dstChainId,
        recipient: to,
        fee: { raw: fee.toString(), value: formatNativeAmount(n.chain, fee) },
        ...receipt,
      });
    }),
  );
}
