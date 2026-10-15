import { hmac } from "@noble/hashes/hmac";
import { sha256 } from "@noble/hashes/sha256";
import { type Address, type Hex, bytesToHex, concat, hexToBytes, numberToBytes } from "viem";

/** A confidential ledger whose account writes carry a modify authorization. */
export type Ledger = "Gratis" | "Promis";

/** Operation tags the enclave binds into the MAC (`crates/system/tee/src/protocol.rs`). */
export const OP = {
  Gratis: { Mint: 0, Burn: 1, Pledge: 2 },
  Promis: { Mint: 0, Burn: 1 },
} as const;

export interface ModifyRequest {
  ledger: Ledger;
  account: Address;
  op: number;
  amount: bigint;
  opNonce: bigint;
  /** The chain id as one 32-byte big-endian word. */
  chainId: Uint8Array;
}

const TAGS: Record<Ledger, Uint8Array> = {
  Gratis: new TextEncoder().encode("outbe/gratis/modify/v1"),
  Promis: new TextEncoder().encode("outbe/promis/modify/v1"),
};

export function chainIdWord(chainId: number | bigint): Uint8Array {
  return numberToBytes(BigInt(chainId), { size: 32 });
}

/** `HMAC-SHA256(modify_key, tag || account || op || amount || opNonce || chainId)`, as the enclave checks it. */
export function modifyMac(modifyKey: Uint8Array, request: ModifyRequest): Hex {
  const preimage = concat([
    TAGS[request.ledger],
    hexToBytes(request.account),
    new Uint8Array([request.op]),
    numberToBytes(request.amount, { size: 32 }),
    numberToBytes(request.opNonce, { size: 8 }),
    request.chainId,
  ]);
  return bytesToHex(hmac(sha256, modifyKey, preimage));
}
