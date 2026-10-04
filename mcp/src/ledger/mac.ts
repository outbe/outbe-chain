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

const U64_MAX = 0xffff_ffff_ffff_ffffn;
const U256_MAX = (1n << 256n) - 1n;

function requireBytes(name: string, hex: Hex, length: number): Uint8Array {
  const bytes = toBytes(hex);
  if (bytes.length !== length) throw new Error(`${name} must be exactly ${length} bytes`);
  return bytes;
}

export function modifyMac(
  domainTag: string,
  modifyKey: Hex,
  account: Address,
  opTag: number,
  amount: bigint,
  opNonce: bigint,
  chainId: Hex,
): Hex {
  if (domainTag !== GRATIS_MODIFY_TAG && domainTag !== PROMIS_MODIFY_TAG) {
    throw new Error("unknown ledger domain tag");
  }
  if (!Number.isInteger(opTag) || opTag < 0 || opTag > 255) throw new Error("opTag must be a byte");
  if (amount < 0n || amount > U256_MAX) throw new Error("amount must fit u256");
  if (opNonce < 0n || opNonce > U64_MAX) throw new Error("opNonce must fit u64");
  const key = requireBytes("modifyKey", modifyKey, 32);
  const preimage = concat([
    new TextEncoder().encode(domainTag),
    requireBytes("account", account, 20),
    new Uint8Array([opTag]),
    toBytes(toHex(amount, { size: 32 })),
    toBytes(toHex(opNonce, { size: 8 })),
    requireBytes("chainId", chainId, 32),
  ]);
  const mac = createHmac("sha256", Buffer.from(key)).update(preimage).digest();
  return toHex(new Uint8Array(mac));
}
