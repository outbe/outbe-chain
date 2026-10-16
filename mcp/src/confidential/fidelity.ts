import { type Address, type Hex, concat, hexToBytes, numberToBytes } from "viem";
import type { LocalAccount } from "viem/accounts";
import { chainIdWord } from "./mac.js";

/** The bytes an account signs to let Fidelity index reads for it through until `expiry`. */
export function fidelityQueryMessage(chainId: number, account: Address, expiry: bigint): Uint8Array {
  return concat([
    new TextEncoder().encode("outbe/fidelity/query-auth/v1"),
    chainIdWord(chainId),
    hexToBytes(account),
    numberToBytes(expiry, { size: 8 }),
  ]);
}

export async function fidelityQueryAuth(account: LocalAccount, chainId: number, expiry: bigint): Promise<Hex> {
  return account.signMessage({ message: { raw: fidelityQueryMessage(chainId, account.address, expiry) } });
}
