import { createHmac } from "node:crypto";
import { type Address, type Hex, concat, toBytes, toHex } from "viem";

/**
 * Confidential-ledger write authorization, verbatim from
 * bin/outbe-tee-enclave/src/confidential.rs (Domain::modify_mac):
 *   mac = HMAC-SHA256(modify_key, domain_tag ++ account[20] ++ op_tag[1] ++ amount_be32 ++ op_nonce_be8 ++ chain_id[32])
 * The wallet holds the per-account modify key the enclave derived for it.
 */
export const GRATIS_MODIFY_TAG = "outbe/gratis/modify/v1";
export const PROMIS_MODIFY_TAG = "outbe/promis/modify/v1";

export function modifyMac(
  domainTag: string,
  modifyKey: Hex,
  account: Address,
  opTag: number,
  amount: bigint,
  opNonce: bigint,
  chainId: Hex,
): Hex {
  const preimage = concat([
    new TextEncoder().encode(domainTag),
    toBytes(account),
    new Uint8Array([opTag]),
    toBytes(toHex(amount, { size: 32 })),
    toBytes(toHex(opNonce, { size: 8 })),
    toBytes(chainId),
  ]);
  const mac = createHmac("sha256", Buffer.from(toBytes(modifyKey))).update(preimage).digest();
  return toHex(new Uint8Array(mac));
}
