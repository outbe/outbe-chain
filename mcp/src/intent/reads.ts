import { type Address, type Hex, decodeAbiParameters, formatUnits } from "viem";
import { formatNativeAmount } from "../chain.js";
import { KNOWN_CHAINS, OUTBE_NETWORK } from "../net/chains.js";
import type { Network, NetworkResolver } from "../net/resolver.js";
import { readDecimals } from "../net/erc20.js";
import { type OrderData, decodeOrderData, isNative } from "./format.js";
import { ERC20_ABI, ROUTER_ABI } from "./registry.js";

/** Current balance of `account` for `token` on a network (native or ERC20). */
export async function tokenBalance(n: Network, token: Address, account: Address) {
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

export interface LoadedOrder {
  origin: Network;
  order: OrderData;
  originData: Hex;
}

/** Read openOrders on a hint network, else probe Outbe and every chain with a known RPC. Then decode the order. */
export async function loadOrder(
  router: Address,
  resolveNetwork: NetworkResolver,
  orderId: Hex,
  hint: Network,
): Promise<LoadedOrder> {
  const candidates: Network[] = [hint];
  for (const name of [OUTBE_NETWORK, ...KNOWN_CHAINS.filter((c) => c.rpc).map((c) => c.name)]) {
    try {
      candidates.push(await resolveNetwork(name));
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
