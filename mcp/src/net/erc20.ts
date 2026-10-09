import { type Abi, type Address, type Hex, encodeFunctionData } from "viem";
import IERC20Json from "../../../contracts/tokens/abi-export/IERC20.json";
import type { Ctx } from "../chain.js";
import type { Network } from "./resolver.js";
import { requireAccount, sendCall, waitForReceipt } from "./tx.js";

export const ERC20_ABI: Abi = IERC20Json as Abi;

export interface Allowance {
  token: Address;
  spender: Address;
  amount: bigint;
}

/**
 * Approves `spender` for `amount` when the signer's allowance is short and waits for
 * the approval to land. Returns its hash, or null when none was needed.
 */
export async function ensureAllowance(ctx: Ctx, n: Network, { token, spender, amount }: Allowance): Promise<Hex | null> {
  const owner = requireAccount(ctx).address;
  const allowance = (await n.client.readContract({
    address: token,
    abi: ERC20_ABI,
    functionName: "allowance",
    args: [owner, spender],
  })) as bigint;
  if (allowance >= amount) return null;
  const data = encodeFunctionData({ abi: ERC20_ABI, functionName: "approve", args: [spender, amount] });
  const hash = await sendCall(ctx, n, { to: token, data, value: 0n });
  const receipt = await waitForReceipt(n, hash);
  if (receipt.status !== "success") throw new Error(`approve ${hash} for ${spender} reverted`);
  return hash;
}
