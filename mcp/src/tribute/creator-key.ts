import { x25519 } from "@noble/curves/ed25519";
import { hkdf } from "@noble/hashes/hkdf";
import { sha256 } from "@noble/hashes/sha256";
import {
  type Address,
  type Hex,
  bytesToHex,
  concat,
  getAddress,
  hexToBigInt,
  hexToBytes,
  numberToBytes,
  parseSignature,
} from "viem";
import type { LocalAccount } from "viem/accounts";

const SALT = new TextEncoder().encode("outbe/mcp/creator-key/v1");
const SECP256K1_ORDER = 0xfffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141n;
const SECP256K1_HALF_ORDER = SECP256K1_ORDER >> 1n;

/** X25519 key the enclave encrypts the creator's Tribute and Nod amounts to. */
export interface CreatorKey {
  secret: Uint8Array;
  publicKey: Hex;
}

export function creatorKeyMessage(chainId: number, address: Address): string {
  return `Outbe Tribute creator key v1 / chain ${chainId} / ${getAddress(address)} / sign only in Outbe tools`;
}

/** Re-derives the same key from the account's signature on every call; nothing is stored. */
export async function creatorKey(account: LocalAccount, chainId: number): Promise<CreatorKey> {
  const signature = await account.signMessage({ message: creatorKeyMessage(chainId, account.address) });
  const { r, s } = parseSignature(signature);
  // Wallets disagree on `v` and may return a high `s`, so only the low-s (r, s) pair feeds the key.
  const sValue = hexToBigInt(s);
  const low = sValue > SECP256K1_HALF_ORDER ? SECP256K1_ORDER - sValue : sValue;
  const ikm = concat([hexToBytes(r, { size: 32 }), numberToBytes(low, { size: 32 })]);
  const info = concat([numberToBytes(chainId, { size: 8 }), hexToBytes(getAddress(account.address))]);
  const secret = hkdf(sha256, ikm, SALT, info, 32);
  return { secret, publicKey: bytesToHex(x25519.getPublicKey(secret)) };
}
